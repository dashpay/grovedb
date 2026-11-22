//! The memory a ring maps is the process's for as long as the ring is, and
//! no longer.
//!
//! The test reads the ranges of addresses the process has mapped. It is
//! alone in its binary, so that no other test makes a ring while it
//! counts. It needs io_uring: Linux 5.6, and a process that is allowed the
//! calls. Where whoever runs the tests says that the system has none
//! (`LIGNIN_IO_URING=absent`), it holds the system to that.
//!
//! Where the system gives rings, a batch of two reads makes one whether
//! or not the ring takes the reads, which it does not where the file
//! system asks for a wider
//! alignment than a ring reads with: the test counts mappings, not the
//! reads of a ring.

#![cfg(all(target_os = "linux", feature = "direct_io"))]

use std::{fs, io::Write};

use lignin_io::{
    linux::{DirectVfs, ReadAt},
    OpenMode, Vfs,
};

/// How many of the process's mappings are of a ring.
fn ring_mappings() -> usize {
    fs::read_to_string("/proc/self/maps")
        .expect("the mappings of the process")
        .lines()
        .filter(|line| line.ends_with("anon_inode:[io_uring]"))
        .count()
}

#[test]
fn the_mappings_of_a_ring_go_when_the_file_layer_goes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("file");
    fs::File::create(&path)
        .unwrap()
        .write_all(&[9; 100_000])
        .unwrap();
    assert_eq!(ring_mappings(), 0);

    let vfs = DirectVfs::new();
    let file = vfs.open(&path, OpenMode::Read).unwrap();
    let mut bufs = [[0; 1000]; 2];
    let [first, second] = &mut bufs;
    let mut reads = [
        ReadAt {
            file: &*file,
            offset: 10,
            buf: first,
        },
        ReadAt {
            file: &*file,
            offset: 50_000,
            buf: second,
        },
    ];
    vfs.read_batch(&mut reads).unwrap();
    assert_eq!(bufs, [[9; 1000]; 2]);
    match std::env::var("LIGNIN_IO_URING").as_deref() {
        // The queues, and the entries of the submission queue.
        Err(_) => assert_eq!(
            ring_mappings(),
            2,
            "this test needs io_uring: LIGNIN_IO_URING=absent says that the system has none"
        ),
        Ok("absent") => {
            assert_eq!(vfs.stats().ring_reads, 0, "there is a ring");
            assert_eq!(ring_mappings(), 0, "there is a ring");
            return;
        }
        Ok(other) => panic!("LIGNIN_IO_URING is `absent` or is not set: {other}"),
    }

    // The ring is kept by the file layer, which the file shares.
    drop(vfs);
    assert_eq!(ring_mappings(), 2);
    drop(file);
    assert_eq!(ring_mappings(), 0);
}
