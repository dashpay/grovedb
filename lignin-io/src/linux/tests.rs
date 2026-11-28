//! The direct I/O path on the files of the system the tests run on.
//!
//! The tests take their directory from `tempfile`, so `TMPDIR` says which
//! file system they run on. It has to take `O_DIRECT`: ext4, XFS and btrfs
//! do, and tmpfs since Linux 6.6. Of those only ext4 and XFS refuse a
//! direct read that is not aligned, so a run there is what shows that
//! every read of this layer is. On every file system the layer itself
//! refuses to make a read that is not ([`DirectFile::pread`]), but for
//! the length of the one that ends at the largest offset a file can have.
//!
//! The tests of batches through a ring need io_uring: Linux 5.6, and a
//! process that is allowed the calls. Where it is refused they fail, unless
//! whoever runs the tests says so with `LIGNIN_IO_URING=absent`, as
//! `TMPDIR` says where the files are. The tests then hold the system to
//! that: they fail if it gives a ring, and each of them ends there.
//! `LIGNIN_IO_URING=present` says what the variable not being set says.
//! CI asks the system before the tests and sets one or the other
//! (`lignin-ci.sh rings`).
//!
//! A ring reads with an alignment of 4,096 bytes at most. On a file system
//! whose files ask for more, the files the tests write go to no ring, and
//! a test of what a ring does with those files ends at once ([`Through`]).
//! The handle that one test makes for itself, with an alignment of 4,096
//! bytes, goes to a ring there too.

use std::{
    io::Write,
    panic::{catch_unwind, AssertUnwindSafe},
    path::PathBuf,
    sync::Barrier,
    thread,
};

use tempfile::TempDir;

use super::*;
use crate::{
    sim::{SimConfig, SimVfs},
    vfs::Create,
};

fn setup() -> (DirectVfs, PathBuf, TempDir) {
    let dir = tempfile::tempdir().unwrap();
    (DirectVfs::new(), dir.path().to_path_buf(), dir)
}

mod conformance {
    use super::setup;

    crate::conformance::conformance_tests!(setup(), host_files);
}

fn content(len: usize, seed: u8) -> Vec<u8> {
    (0..len)
        .map(|at| (at as u8).wrapping_mul(13).wrapping_add((at >> 8) as u8) ^ seed)
        .collect()
}

/// A file of `content` under `name`, written as the std backend writes.
fn written(root: &Path, name: &str, content: &[u8]) -> PathBuf {
    let path = root.join(name);
    let mut file = File::create(&path).unwrap();
    file.write_all(content).unwrap();
    file.sync_all().unwrap();
    path
}

fn direct(vfs: &DirectVfs, path: &Path) -> DirectFile {
    DirectFile::open(path, vfs.shared.clone()).unwrap()
}

/// The reads since `before`.
fn since(vfs: &DirectVfs, before: DirectStats) -> DirectStats {
    let now = vfs.stats();
    DirectStats {
        preads: now.preads - before.preads,
        ring_calls: now.ring_calls - before.ring_calls,
        ring_reads: now.ring_reads - before.ring_reads,
        rings_lost: now.rings_lost - before.rings_lost,
        bytes: now.bytes - before.bytes,
    }
}

/// What a batch of the files of the tests' directory is made through.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Through {
    /// A ring: the system gives rings, and the files take them.
    Ring,
    /// Single reads: the system gives no ring, as whoever runs the tests
    /// said.
    NoRing,
    /// Single reads, though the system gives rings: the file system asks
    /// for a wider alignment than a ring reads with.
    WideFiles,
}

fn through() -> Through {
    let dir = tempfile::tempdir().unwrap();
    let probe = written(dir.path(), "probe", &[0; 16]);
    let narrow = DirectFile::open(&probe, Arc::default()).unwrap().align <= KEPT_ALIGN;
    match (ring::system_gives_rings(), narrow) {
        (false, _) => Through::NoRing,
        (true, true) => Through::Ring,
        (true, false) => Through::WideFiles,
    }
}

/// Whether batches of the tests' files go through a ring. A test of what
/// a ring does with them ends where this is `false`.
fn rings() -> bool {
    through() == Through::Ring
}

fn is_direct(file: &dyn VfsFile) -> bool {
    let file: &dyn Any = file;
    file.is::<DirectFile>()
}

#[test]
fn a_file_opened_to_read_is_direct_and_one_opened_to_write_is_the_std_backends() {
    let (vfs, root, _dir) = setup();
    let path = written(&root, "file", &content(10_000, 1));
    assert!(is_direct(&*vfs.open(&path, OpenMode::Read).unwrap()));
    for mode in [
        OpenMode::Write(Create::Never),
        OpenMode::Write(Create::IfMissing),
        OpenMode::Append(Create::Never),
    ] {
        let file = vfs.open(&path, mode).unwrap();
        assert!(!is_direct(&*file), "{mode:?}");
        // What is read through it is not counted: it is not this layer's.
        let before = vfs.stats();
        let mut buf = [0; 100];
        file.read_exact_at(&mut buf, 50).unwrap();
        assert_eq!(buf, content(10_000, 1)[50..150]);
        assert_eq!(since(&vfs, before), DirectStats::default());
    }
    assert!(format!("{vfs:?}").starts_with("DirectVfs"));
    assert!(format!("{:?}", direct(&vfs, &path)).starts_with("DirectFile"));
}

#[test]
fn a_read_is_one_aligned_read_of_the_range_around_it() {
    let (vfs, root, _dir) = setup();
    let all = content(300_000, 2);
    let file = direct(&vfs, &written(&root, "file", &all));
    let align = file.align.get();
    for (offset, len) in [
        (0u64, 1usize),
        (1, 1),
        (0, 4096),
        (4095, 2),
        (10_000, 30_000),
        (123_457, 100_001),
        (299_999, 1),
    ] {
        let before = vfs.stats();
        let mut buf = vec![0; len];
        file.read_exact_at(&mut buf, offset).unwrap();
        assert_eq!(buf, all[offset as usize..offset as usize + len]);
        let made = since(&vfs, before);
        assert_eq!(made.preads, 1, "{offset} {len}");
        let from = offset / align * align;
        let to = (offset + len as u64).div_ceil(align) * align;
        assert_eq!(made.bytes, to.min(300_000) - from, "{offset} {len}");
        assert_eq!((made.ring_calls, made.ring_reads), (0, 0));
    }
}

#[test]
fn a_read_that_reaches_the_end_of_the_file_asks_for_the_length() {
    let (vfs, root, _dir) = setup();
    let all = content(10_000, 3);
    let file = direct(&vfs, &written(&root, "file", &all));
    for (offset, len, read) in [
        (0, 20_000, 10_000),
        (9_999, 2, 1),
        (10_000, 1, 0),
        (50_000, 10, 0),
    ] {
        let before = vfs.stats();
        let mut buf = vec![0; len];
        assert_eq!(file.read_at(&mut buf, offset).unwrap(), read);
        assert_eq!(buf[..read], all[(offset as usize).min(10_000)..][..read]);
        assert_eq!(since(&vfs, before).preads, 1, "{offset} {len}");
        let error = file.read_exact_at(&mut buf, offset).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
    }
    assert_eq!(file.read_at(&mut [], 5).unwrap(), 0);
    assert_eq!(file.read_at(&mut [0; 8], u64::MAX).unwrap(), 0);
    assert_eq!(file.len().unwrap(), 10_000);
}

#[test]
fn a_long_read_is_made_in_pieces_and_a_wide_alignment_in_memory_of_its_own() {
    let (vfs, root, _dir) = setup();
    let all = content(3 << 20, 4);
    let path = written(&root, "file", &all);
    let file = direct(&vfs, &path);

    let before = vfs.stats();
    let mut buf = vec![0; all.len() - 777];
    file.read_exact_at(&mut buf, 300).unwrap();
    assert_eq!(buf, all[300..all.len() - 477]);
    assert_eq!(since(&vfs, before).preads, 3);

    // A multiple of what the file needs is an alignment it can be read
    // with, and wider than the memory this layer keeps.
    for wide in [8192, 65536, 1 << 20] {
        let mut file = direct(&vfs, &path);
        file.align = Align::new(wide).unwrap().max(file.align);
        let before = vfs.stats();
        let mut buf = vec![0; 100_000];
        file.read_exact_at(&mut buf, 70_001).unwrap();
        assert_eq!(buf, all[70_001..170_001]);
        let made = since(&vfs, before);
        assert_eq!(made.preads, 1);
        assert_eq!(made.bytes % wide, 0);
    }
}

#[test]
fn what_cannot_be_read_directly_is_refused_when_it_is_opened() {
    let (vfs, root, _dir) = setup();
    let missing = vfs.open(&root.join("missing"), OpenMode::Read).unwrap_err();
    assert_eq!(missing.kind(), ErrorKind::NotFound);

    // The file system of `/proc` takes no direct I/O.
    let error = vfs
        .open(Path::new("/proc/self/stat"), OpenMode::Read)
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Unsupported, "{error}");
    assert!(error.to_string().contains("/proc/self/stat"), "{error}");
    let error = vfs
        .open(Path::new("/proc/self"), OpenMode::Read)
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::IsADirectory, "{error}");
    let error = vfs.open(&root, OpenMode::Read).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::IsADirectory, "{error}");

    // What the system reports of a file decides.
    assert_eq!(alignment(None), Some(Align::DEFAULT));
    assert_eq!(alignment(Some((512, 512))), Align::new(512));
    assert_eq!(alignment(Some((4, 4096))), Align::new(4096));
    assert_eq!(alignment(Some((0, 0))), None);
    assert_eq!(alignment(Some((512, 3 << 20))), None);

    // And for a file that is there, it is what the system says, or the
    // default.
    let path = written(&root, "file", &content(100, 5));
    let found = statx(
        File::open(&path).unwrap(),
        "",
        AtFlags::EMPTY_PATH,
        StatxFlags::DIOALIGN,
    )
    .unwrap();
    let says = StatxFlags::from_bits_retain(found.stx_mask).contains(StatxFlags::DIOALIGN);
    let expected = if says {
        Align::reported(found.stx_dio_mem_align, found.stx_dio_offset_align).unwrap()
    } else {
        Align::DEFAULT
    };
    assert_eq!(direct(&vfs, &path).align, expected);
}

#[test]
#[should_panic(expected = "is not aligned")]
fn a_read_that_is_not_aligned_is_not_made() {
    let (vfs, root, _dir) = setup();
    let file = direct(&vfs, &written(&root, "file", &content(10_000, 6)));
    let mut memory = vec![0; 2 * file.align.get() as usize];
    let _ = file.pread(&mut memory[1..], 0);
}

#[test]
fn a_direct_handle_writes_nothing() {
    let (vfs, root, _dir) = setup();
    let file = direct(&vfs, &written(&root, "file", &content(100, 7)));
    for error in [
        file.write_at(b"x", 0).unwrap_err(),
        file.append(b"x").unwrap_err(),
        file.set_len(5).unwrap_err(),
    ] {
        assert_eq!(error.kind(), ErrorKind::InvalidInput);
        assert!(error.to_string().contains("opened to write"), "{error}");
    }
    assert_eq!(file.len().unwrap(), 100);
}

#[test]
fn a_sync_that_failed_keeps_failing_on_its_handle() {
    let (vfs, root, _dir) = setup();
    let path = written(&root, "file", &content(100, 8));
    let file = direct(&vfs, &path);
    file.sync_data().unwrap();
    file.sync_all().unwrap();

    let error = file
        .sync_using(|_| Err(ErrorKind::StorageFull.into()))
        .unwrap_err();
    assert_eq!(error.kind(), ErrorKind::StorageFull);
    for error in [file.sync_data().unwrap_err(), file.sync_all().unwrap_err()] {
        assert_eq!(error.kind(), ErrorKind::Other);
        assert!(error.to_string().contains("an earlier sync"), "{error}");
    }
    // Another handle of the file is not stopped, and neither are reads.
    direct(&vfs, &path).sync_data().unwrap();
    let mut buf = [0; 10];
    file.read_exact_at(&mut buf, 0).unwrap();

    let file = direct(&vfs, &path);
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        let _ = file.sync_using(|_| panic!("in a sync"));
    }));
    assert!(panicked.is_err());
    let error = file.sync_data().unwrap_err();
    assert_eq!(error.kind(), ErrorKind::Other);
    assert!(error.to_string().contains("panicked"), "{error}");
}

/// A list of reads over `files`, each with the bytes it has to bring.
fn plan<'a>(
    files: &'a [(Box<dyn VfsFile>, Vec<u8>)],
    count: usize,
    bufs: &'a mut Vec<Vec<u8>>,
) -> (Vec<ReadAt<'a>>, Vec<&'a [u8]>) {
    let wanted: Vec<(usize, usize, usize)> = (0..count)
        .map(|index| {
            let (_, all) = &files[index % files.len()];
            let len = 1 + (index * 7919) % 70_000.min(all.len());
            let offset = (index * 104_729) % (all.len() - len + 1);
            (index % files.len(), offset, len)
        })
        .collect();
    *bufs = wanted.iter().map(|(_, _, len)| vec![0; *len]).collect();
    let expected = wanted
        .iter()
        .map(|(file, offset, len)| &files[*file].1[*offset..offset + len])
        .collect();
    let reads = wanted
        .iter()
        .zip(bufs.iter_mut())
        .map(|((file, offset, _), buf)| ReadAt {
            file: &*files[*file].0,
            offset: *offset as u64,
            buf,
        })
        .collect();
    (reads, expected)
}

fn direct_files(vfs: &DirectVfs, root: &Path, count: usize) -> Vec<(Box<dyn VfsFile>, Vec<u8>)> {
    (0..count)
        .map(|index| {
            let all = content(200_000 + 33_333 * index, index as u8);
            let path = written(root, &format!("file-{index}"), &all);
            (vfs.open(&path, OpenMode::Read).unwrap(), all)
        })
        .collect()
}

fn assert_read(reads: &[ReadAt<'_>], expected: &[&[u8]]) {
    for (index, (read, expected)) in reads.iter().zip(expected).enumerate() {
        assert!(&*read.buf == *expected, "read {index} at {}", read.offset);
    }
}

#[test]
fn a_batch_goes_to_a_ring_and_brings_what_the_single_reads_bring() {
    if !rings() {
        return;
    }
    let (vfs, root, _dir) = setup();
    let files = direct_files(&vfs, &root, 3);
    for count in [2, 5, 32, 33, 100] {
        let mut bufs = Vec::new();
        let (mut reads, expected) = plan(&files, count, &mut bufs);
        // Every read a ring takes goes to it, and no other.
        let taken = reads.iter().filter(|read| ring_read(read).is_some());
        let taken = taken.count() as u64;
        assert!(taken > 0);
        let before = vfs.stats();
        vfs.read_batch(&mut reads).unwrap();
        assert_read(&reads, &expected);
        let made = since(&vfs, before);
        assert_eq!(made.ring_reads, taken, "the ring's reads went to it");
        assert_eq!(made.preads, count as u64 - taken, "and none was made again");
        assert!(made.ring_calls >= taken.div_ceil(ring::DEPTH as u64));
        assert!(made.bytes >= expected.iter().map(|bytes| bytes.len() as u64).sum());
    }
    assert_eq!(vfs.shared.rings().kept.len(), 1, "one ring, kept");
    assert!(format!("{:?}", vfs.shared.rings()).contains("Ring"));
}

#[test]
fn without_a_ring_a_batch_is_the_single_reads() {
    let (_, root, _dir) = setup();
    let vfs = DirectVfs::without_rings();
    let files = direct_files(&vfs, &root, 3);
    for count in [0, 1, 2, 40] {
        let mut bufs = Vec::new();
        let (mut reads, expected) = plan(&files, count, &mut bufs);
        let before = vfs.stats();
        vfs.read_batch(&mut reads).unwrap();
        assert_read(&reads, &expected);
        let made = since(&vfs, before);
        assert_eq!((made.ring_calls, made.ring_reads), (0, 0));
        assert_eq!(made.preads, count as u64);
    }

    // One read is not worth a ring where there is one.
    let (vfs, ..) = setup();
    let files = direct_files(&vfs, &root, 1);
    let mut bufs = Vec::new();
    let (mut reads, expected) = plan(&files, 1, &mut bufs);
    vfs.read_batch(&mut reads).unwrap();
    assert_read(&reads, &expected);
    let made = vfs.stats();
    assert_eq!((made.preads, made.ring_calls, made.ring_reads), (1, 0, 0));
    assert!(vfs.shared.rings().kept.is_empty());
}

#[test]
fn a_batch_takes_any_file_and_any_read() {
    if !rings() {
        return;
    }
    let (vfs, root, _dir) = setup();
    let all = content(500_000, 9);
    let path = written(&root, "file", &all);
    let direct_file = vfs.open(&path, OpenMode::Read).unwrap();
    let std_file = vfs.open(&path, OpenMode::Write(Create::Never)).unwrap();
    let mut wide = direct(&vfs, &path);
    wide.align = Align::new(65536).unwrap();
    let sim = SimVfs::new(SimConfig::new(1));
    let sim_file = sim
        .open(Path::new("/file"), OpenMode::Append(Create::New))
        .unwrap();
    sim_file.append(&all[..5000]).unwrap();

    let mut bufs: Vec<Vec<u8>> = [100, 0, 200_000, 300, 5000, 1000, 400, 4096]
        .iter()
        .map(|len| vec![0; *len])
        .collect();
    let offsets = [7u64, 9, 1000, 20_000, 0, 499_000, 65_000, 8192];
    let files: [&dyn VfsFile; 8] = [
        &*direct_file,
        &*direct_file,
        &*direct_file,
        &*std_file,
        &*sim_file,
        &*direct_file,
        &wide,
        &*direct_file,
    ];
    let mut reads: Vec<ReadAt<'_>> = files
        .into_iter()
        .zip(&offsets)
        .zip(bufs.iter_mut())
        .map(|((file, offset), buf)| ReadAt {
            file,
            offset: *offset,
            buf,
        })
        .collect();
    assert!(format!("{:?}", reads[0]).starts_with("ReadAt"));
    let before = vfs.stats();
    vfs.read_batch(&mut reads).unwrap();
    for (read, offset) in reads.iter().zip(&offsets) {
        assert_eq!(
            &*read.buf,
            &all[*offset as usize..*offset as usize + read.buf.len()]
        );
    }
    let made = since(&vfs, before);
    // The ring is given the reads of direct files that fit its memory:
    // not the empty one, not the long one, not the one with the wide
    // alignment, and not those of other file layers.
    assert_eq!(made.ring_reads, 3);
    assert_eq!(made.preads, 2);
}

#[test]
fn a_batch_fails_as_the_read_that_fails() {
    let (vfs, root, _dir) = setup();
    let files = direct_files(&vfs, &root, 2);
    let len = files[0].1.len() as u64;
    for with_ring in [true, false] {
        let vfs = if with_ring {
            vfs.clone()
        } else {
            DirectVfs::without_rings()
        };
        let mut bufs = [vec![0; 100], vec![0; 100], vec![0; 100]];
        let [first, second, third] = &mut bufs;
        let mut reads = [
            ReadAt {
                file: &*files[0].0,
                offset: 5,
                buf: first,
            },
            ReadAt {
                file: &*files[0].0,
                offset: len - 50,
                buf: second,
            },
            ReadAt {
                file: &*files[1].0,
                offset: 5,
                buf: third,
            },
        ];
        let error = vfs.read_batch(&mut reads).unwrap_err();
        assert_eq!(error.kind(), ErrorKind::UnexpectedEof);
        // A read that ends where the file ends is whole.
        reads[1].offset = len - 100;
        vfs.read_batch(&mut reads).unwrap();
        assert_eq!(&*reads[1].buf, &files[0].1[len as usize - 100..]);
    }
}

#[test]
fn a_batch_fails_with_the_error_the_ring_answered_and_does_not_ask_twice() {
    use rustix::io::Errno;

    let (vfs, root, _dir) = setup();
    let files = direct_files(&vfs, &root, 1);
    // A handle that cannot be read: every read of it fails with `EBADF`.
    let path = written(&root, "unreadable", &content(10_000, 12));
    let unreadable = DirectFile {
        file: OpenOptions::new().write(true).open(&path).unwrap(),
        align: Align::DEFAULT,
        shared: vfs.shared.clone(),
        sync_failure: Mutex::new(None),
    };
    let mut bufs = [vec![0; 100], vec![0; 100], vec![0; 100]];
    let [first, second, third] = &mut bufs;
    let mut reads = [
        ReadAt {
            file: &*files[0].0,
            offset: 5,
            buf: first,
        },
        ReadAt {
            file: &unreadable,
            offset: 5,
            buf: second,
        },
        ReadAt {
            file: &*files[0].0,
            offset: 700,
            buf: third,
        },
    ];
    let before = vfs.stats();
    let error = vfs.read_batch(&mut reads).unwrap_err();
    assert_eq!(error.raw_os_error(), Some(Errno::BADF.raw_os_error()));
    let made = since(&vfs, before);
    match through() {
        Through::Ring => {
            // The ring's answer is the read's error: no single read is
            // made of it. The two reads beside it are counted with their
            // bytes, the aligned range of each, and both were copied out
            // though the batch failed at the read between them.
            assert_eq!((made.ring_reads, made.preads), (3, 0));
            let align = direct(&vfs, &root.join("file-0")).align;
            let range = |offset| {
                let (from, to) = span(offset, 100, align).unwrap();
                to - from
            };
            assert_eq!(made.bytes, range(5) + range(700), "{made:?}");
            assert_eq!(&*reads[2].buf, &files[0].1[700..800]);
        }
        Through::NoRing => assert_eq!((made.ring_reads, made.preads), (0, 2)),
        // The handle that was made here goes to a ring alone.
        Through::WideFiles => assert_eq!(made.ring_reads, 1),
    }
    assert_eq!(&*reads[0].buf, &files[0].1[5..105]);
}

/// A file of another file layer whose read panics.
#[derive(Debug)]
struct Panics;

impl VfsFile for Panics {
    fn read_at(&self, _: &mut [u8], _: u64) -> io::Result<usize> {
        panic!("a read that panics")
    }

    fn write_at(&self, _: &[u8], _: u64) -> io::Result<()> {
        unreachable!()
    }

    fn append(&self, _: &[u8]) -> io::Result<u64> {
        unreachable!()
    }

    fn sync_data(&self) -> io::Result<()> {
        unreachable!()
    }

    fn sync_all(&self) -> io::Result<()> {
        unreachable!()
    }

    fn len(&self) -> io::Result<u64> {
        unreachable!()
    }

    fn set_len(&self, _: u64) -> io::Result<()> {
        unreachable!()
    }
}

#[test]
fn the_ring_is_done_with_before_a_file_of_the_list_is_read() {
    if !rings() {
        return;
    }
    let (vfs, root, _dir) = setup();
    let files = direct_files(&vfs, &root, 1);
    let batch = |bufs: &mut [Vec<u8>; 2]| {
        let [first, second] = bufs;
        let mut reads = [
            ReadAt {
                file: &Panics,
                offset: 0,
                buf: first,
            },
            ReadAt {
                file: &*files[0].0,
                offset: 900,
                buf: second,
            },
        ];
        catch_unwind(AssertUnwindSafe(|| vfs.read_batch(&mut reads)))
    };

    // The read of the first file panics. What the ring brought for the
    // second was counted and is in its buffer by then.
    let in_the_read = |outcome: thread::Result<io::Result<()>>| {
        outcome.unwrap_err().downcast_ref::<&str>() == Some(&"a read that panics")
    };
    let mut bufs = [vec![0; 10], vec![0; 300]];
    assert!(in_the_read(batch(&mut bufs)));
    assert_eq!(bufs[1], files[0].1[900..1200]);
    let made = vfs.stats();
    assert_eq!((made.ring_reads, made.rings_lost), (1, 0));

    // With a ring that breaks, the ring is counted as lost, and as one of
    // the three after which no further is made, before the panic.
    for lost in 1..=RING_FAILURES {
        vfs.shared
            .rings()
            .kept
            .push(Ring::broken().expect("a ring"));
        let mut bufs = [vec![0; 10], vec![0; 300]];
        assert!(in_the_read(batch(&mut bufs)));
        assert_eq!(vfs.stats().rings_lost, u64::from(lost));
        assert_eq!(vfs.shared.rings().failures, lost);
        assert!(vfs.shared.rings().kept.is_empty());
    }
    assert!(vfs.shared.take_ring().is_none());
}

#[test]
fn a_ring_that_breaks_is_given_up_and_its_reads_are_made_one_by_one() {
    if !rings() {
        return;
    }
    let (vfs, root, _dir) = setup();
    let files = direct_files(&vfs, &root, 2);
    vfs.shared
        .rings()
        .kept
        .push(Ring::broken().expect("a ring"));
    let mut bufs = Vec::new();
    let (mut reads, expected) = plan(&files, 40, &mut bufs);
    let before = vfs.stats();
    vfs.read_batch(&mut reads).unwrap();
    assert_read(&reads, &expected);
    let made = since(&vfs, before);
    // One call, which failed with nothing taken.
    assert_eq!((made.ring_calls, made.ring_reads), (1, 0));
    assert_eq!((made.preads, made.rings_lost), (40, 1));
    assert!(vfs.shared.rings().kept.is_empty(), "the ring is not kept");

    // The next batch makes a ring of its own.
    let (mut reads, expected) = plan(&files, 40, &mut bufs);
    vfs.read_batch(&mut reads).unwrap();
    assert_read(&reads, &expected);
    assert_eq!(since(&vfs, before).ring_reads, 40);
}

#[test]
fn a_ring_of_another_process_is_given_up_and_its_reads_are_made_one_by_one() {
    if !rings() {
        return;
    }
    let (vfs, root, _dir) = setup();
    let files = direct_files(&vfs, &root, 2);
    vfs.shared
        .rings()
        .kept
        .push(Ring::foreign().expect("a ring"));
    let mut bufs = Vec::new();
    let (mut reads, expected) = plan(&files, 40, &mut bufs);
    let before = vfs.stats();
    vfs.read_batch(&mut reads).unwrap();
    assert_read(&reads, &expected);
    let made = since(&vfs, before);
    assert_eq!((made.ring_calls, made.ring_reads), (0, 0));
    assert_eq!((made.preads, made.rings_lost), (40, 1));
    assert!(vfs.shared.rings().kept.is_empty(), "the ring is not kept");
}

#[test]
fn a_read_is_settled_by_what_became_of_it_in_the_ring() {
    use rustix::io::Errno;

    let bytes = content(8192, 11);
    let whole = |skip, len| match settle(Outcome::Answered(Ok(&bytes)), skip, len) {
        Settled::Whole(found) => Some(found.to_vec()),
        Settled::Again => None,
        Settled::Failed(error) => panic!("{error}"),
    };
    assert_eq!(whole(0, 8192), Some(bytes.clone()));
    assert_eq!(whole(100, 5000), Some(bytes[100..5100].to_vec()));
    assert_eq!(whole(8191, 1), Some(bytes[8191..].to_vec()));
    assert_eq!(whole(0, 0), Some(Vec::new()));
    // Fewer bytes than the read needs: the single read says where the
    // file ends.
    assert_eq!(whole(0, 8193), None);
    assert_eq!(whole(8000, 193), None);
    assert_eq!(whole(9000, 1), None);
    assert_eq!(whole(u64::MAX, 1), None);
    assert_eq!(whole(1, usize::MAX), None);
    let nothing = settle(Outcome::Answered(Ok(&[])), 0, 1);
    assert!(matches!(nothing, Settled::Again));

    // An error is the read's own, but for the three that say that the
    // read was not made.
    let answered = |errno: Errno| settle(Outcome::Answered(Err(io::Error::from(errno))), 0, 100);
    for errno in [Errno::AGAIN, Errno::INTR, Errno::CANCELED] {
        assert!(matches!(answered(errno), Settled::Again), "{errno:?}");
    }
    for errno in [Errno::IO, Errno::NOSPC, Errno::INVAL, Errno::BADF] {
        let Settled::Failed(error) = answered(errno) else {
            panic!("{errno:?} is the read's error");
        };
        assert_eq!(error.raw_os_error(), Some(errno.raw_os_error()));
    }
    let other = settle(
        Outcome::Answered(Err(io::Error::other("no number"))),
        0,
        100,
    );
    assert!(matches!(other, Settled::Failed(_)), "{other:?}");

    // A read the system did not take is made by the single read. One it
    // took and did not answer is not made again: what it met is not
    // known.
    assert!(matches!(settle(Outcome::NotMade, 0, 100), Settled::Again));
    let Settled::Failed(unknown) = settle(Outcome::Unknown, 0, 100) else {
        panic!("a read of which nothing is known fails");
    };
    assert_eq!(unknown.kind(), ErrorKind::Other);
    assert!(unknown.to_string().contains("not known"), "{unknown}");
}

#[test]
fn a_path_the_system_never_saw_is_refused_as_what_it_is() {
    let (vfs, root, _dir) = setup();
    // No call is made for a name with a zero byte in it: the error is
    // the name's, and says nothing of direct I/O.
    let error = vfs.open(&root.join("a\0b"), OpenMode::Read).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::InvalidInput, "{error}");
    assert!(!error.to_string().contains("direct"), "{error}");
}

#[test]
fn a_read_fails_where_the_system_gives_no_memory_for_it() {
    let (vfs, root, _dir) = setup();
    let file = direct(&vfs, &written(&root, "file", &content(10_000, 13)));
    let mut buf = [0; 100];
    aligned::fail_the_next_buffer();
    let error = file.read_at(&mut buf, 5).unwrap_err();
    assert_eq!(error.kind(), ErrorKind::OutOfMemory);
    assert_eq!(vfs.stats().preads, 0);
    file.read_exact_at(&mut buf, 5).unwrap();
    assert_eq!(buf, content(10_000, 13)[5..105]);
}

#[test]
fn the_last_bytes_a_file_can_have_are_read_or_refused_and_never_missed() {
    let (vfs, root, _dir) = setup();
    let path = root.join("file");
    let writer = vfs.open(&path, OpenMode::Write(Create::New)).unwrap();
    // A file with nothing in it, as long as a file can be, where the
    // file system has files that long.
    let longest = i64::MAX as u64;
    if writer.set_len(longest).is_err() {
        return;
    }
    let file = direct(&vfs, &path);
    assert_eq!(file.len().unwrap(), longest);
    let mut through_the_cache = [1; 16];
    let there = writer.read_at(&mut through_the_cache, longest - 5).unwrap();
    assert_eq!((there, &through_the_cache[..5]), (5, &[0; 5][..]));
    let mut buf = [1; 16];
    match file.read_at(&mut buf, longest - 5) {
        Ok(read) => assert_eq!((read, &buf[..5]), (5, &[0; 5][..])),
        // The call for the last block is not a multiple of the block
        // size long, and a file system may insist.
        Err(error) => assert_eq!(error.kind(), ErrorKind::InvalidInput, "{error}"),
    }
    assert_eq!(file.read_at(&mut buf, longest).unwrap(), 0);
}

#[test]
fn rings_are_kept_up_to_a_number() {
    if !rings() {
        return;
    }
    let (vfs, ..) = setup();
    let taken: Vec<Ring> = (0..KEPT_RINGS + 3)
        .map(|_| vfs.shared.take_ring().expect("a ring"))
        .collect();
    for ring in taken {
        vfs.shared.give_ring(ring);
    }
    assert_eq!(vfs.shared.rings().kept.len(), KEPT_RINGS);
}

#[test]
fn after_three_rings_that_failed_no_further_ring_is_made() {
    let none = DirectVfs::without_rings();
    assert!(none.shared.take_ring().is_none());
    assert!(none.shared.rings().kept.is_empty());
    let through = through();
    if through == Through::WideFiles {
        return;
    }
    if through == Through::NoRing {
        // Each batch asks the system, which gives no ring, until it has
        // been asked three times.
        let (vfs, root, _dir) = setup();
        let files = direct_files(&vfs, &root, 1);
        for asked in 1..=5 {
            let mut bufs = Vec::new();
            let (mut reads, expected) = plan(&files, 4, &mut bufs);
            vfs.read_batch(&mut reads).unwrap();
            assert_read(&reads, &expected);
            assert_eq!(vfs.shared.rings().failures, asked.min(RING_FAILURES));
        }
        return;
    }

    let (vfs, root, _dir) = setup();
    let files = direct_files(&vfs, &root, 2);
    for lost in 1..=RING_FAILURES {
        vfs.shared
            .rings()
            .kept
            .push(Ring::broken().expect("a ring"));
        let mut bufs = Vec::new();
        let (mut reads, expected) = plan(&files, 5, &mut bufs);
        vfs.read_batch(&mut reads).unwrap();
        assert_read(&reads, &expected);
        assert_eq!(vfs.stats().rings_lost, u64::from(lost));
    }
    // No ring is made now, so a batch is the single reads.
    assert!(vfs.shared.take_ring().is_none());
    let mut bufs = Vec::new();
    let (mut reads, expected) = plan(&files, 5, &mut bufs);
    let before = vfs.stats();
    vfs.read_batch(&mut reads).unwrap();
    assert_read(&reads, &expected);
    assert_eq!(
        (since(&vfs, before).preads, since(&vfs, before).ring_reads),
        (5, 0)
    );

    // A ring that is kept is still used.
    vfs.shared.give_ring(Ring::new().expect("a ring"));
    let (mut reads, expected) = plan(&files, 5, &mut bufs);
    let before = vfs.stats();
    vfs.read_batch(&mut reads).unwrap();
    assert_read(&reads, &expected);
    assert_eq!(since(&vfs, before).ring_reads, 5);
    assert_eq!(vfs.shared.rings().kept.len(), 1);
}

#[test]
fn threads_read_batches_at_once() {
    let (vfs, root, _dir) = setup();
    let files = direct_files(&vfs, &root, 4);
    let threads = 6;
    let start = Barrier::new(threads);
    thread::scope(|scope| {
        for thread in 0..threads {
            let (vfs, files, start) = (&vfs, &files, &start);
            scope.spawn(move || {
                start.wait();
                for round in 0..20 {
                    let mut bufs = Vec::new();
                    let (mut reads, expected) = plan(files, 3 + thread * 11 + round, &mut bufs);
                    vfs.read_batch(&mut reads).unwrap();
                    assert_read(&reads, &expected);
                }
            });
        }
    });
    assert!(vfs.shared.rings().kept.len() <= threads.min(KEPT_RINGS));
    if rings() {
        let made = vfs.stats();
        assert_eq!((made.preads, made.rings_lost), (0, 0));
    }
}

#[test]
fn a_direct_read_sees_what_was_appended_through_another_handle() {
    let (vfs, root, _dir) = setup();
    let path = root.join("file");
    let appender = vfs.open(&path, OpenMode::Append(Create::New)).unwrap();
    let reader = vfs.open(&path, OpenMode::Read).unwrap();
    let all = content(100_000, 10);
    let mut end = 0;
    // Pieces of any length, none of them synced: each is read back at
    // once, alone and with what came before it.
    for len in [1, 511, 4096, 3, 30_000, 12_345, 4097] {
        assert_eq!(appender.append(&all[end..end + len]).unwrap(), end as u64);
        let mut last = vec![0; len];
        reader.read_exact_at(&mut last, end as u64).unwrap();
        assert_eq!(last, all[end..end + len]);
        end += len;
        let mut whole = vec![0; end + 10];
        assert_eq!(reader.read_at(&mut whole, 0).unwrap(), end);
        assert_eq!(whole[..end], all[..end]);
        assert_eq!(reader.len().unwrap(), end as u64);
    }
    appender.sync_data().unwrap();
    appender.set_len(1000).unwrap();
    let mut whole = vec![0; 5000];
    assert_eq!(reader.read_at(&mut whole, 0).unwrap(), 1000);
    assert_eq!(whole[..1000], all[..1000]);
}
