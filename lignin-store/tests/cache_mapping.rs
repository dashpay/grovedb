//! The frames of a large pool are a mapping of the pool's own on Linux
//! (x86-64 and AArch64) and on macOS: there while the pool lives, and gone
//! when it is dropped.
//!
//! On Linux the test reads the ranges of addresses the process has mapped,
//! and looks for the frames' own range among them. Between the drop of the
//! pool and that reading it allocates nothing and opens nothing: the file
//! is open and the buffer is there before. On macOS, which has no such
//! list, it reads the size of the process's address space through `ps`,
//! which is a looser test.
//!
//! It is the one test of its file, so that its thread is the only one of
//! the process that maps or allocates while it measures. Miri has neither
//! thing to read, and does not report a mapping that is never unmapped.

#![cfg(not(miri))]

use std::{fs::File, io::Read};

use lignin_format::PageSize;
use lignin_store::cache::{Blank, BufferPool, PoolConfig};

/// Whether this build maps the frames of a large pool: Linux on x86-64 and
/// on AArch64, and macOS, each with 64-bit pointers.
const MAPS: bool = cfg!(all(
    target_pointer_width = "64",
    any(
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
        target_os = "macos",
    ),
));

/// The ranges of addresses a process has mapped, on Linux: read from an
/// open file into a buffer that is long enough, with no allocation.
struct Mappings {
    file: File,
    text: Vec<u8>,
    len: usize,
}

impl Mappings {
    fn ready() -> Self {
        Self {
            file: File::open("/proc/self/maps").expect("the mappings"),
            text: vec![0; 1 << 20],
            len: 0,
        }
    }

    fn read(&mut self) {
        loop {
            let read = self
                .file
                .read(&mut self.text[self.len..])
                .expect("the mappings");
            if read == 0 {
                return;
            }
            self.len += read;
            assert!(self.len < self.text.len(), "the buffer holds the mappings");
        }
    }

    /// Each line begins with two addresses in hexadecimal and a hyphen
    /// between them. The rest of a line can be a path that is not text.
    fn ranges(&self) -> Vec<(u64, u64)> {
        self.text[..self.len]
            .split(|&byte| byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| {
                let range = line.split(|&byte| byte == b' ').next().expect("a range");
                let range = std::str::from_utf8(range).expect("two addresses");
                let (start, end) = range.split_once('-').expect("two addresses");
                let address = |text| u64::from_str_radix(text, 16).expect("an address");
                (address(start), address(end))
            })
            .collect()
    }
}

/// Whether every address from `start` up to `end` is in one of the ranges,
/// which are in the order of their addresses.
fn covers(ranges: &[(u64, u64)], start: u64, end: u64) -> bool {
    let mut reached = start;
    for &(from, to) in ranges {
        if from <= reached && reached < to {
            reached = to;
        }
    }
    reached >= end
}

/// Whether any address from `start` up to `end` is in one of the ranges.
fn touches(ranges: &[(u64, u64)], start: u64, end: u64) -> bool {
    ranges.iter().any(|&(from, to)| from < end && start < to)
}

/// The size of the process's address space in bytes, on macOS.
fn address_space() -> u64 {
    let asked = std::process::Command::new("ps")
        .args(["-o", "vsz=", "-p", &std::process::id().to_string()])
        .output()
        .expect("ps runs");
    let text = String::from_utf8(asked.stdout).expect("text");
    let kib: u64 = text.trim().parse().expect("a number");
    kib * 1024
}

#[test]
fn the_frames_of_a_large_pool_are_mapped_while_it_lives_and_unmapped_with_it() {
    // What the two readings of the ranges are judged by.
    let ranges = [(10, 20), (20, 30), (40, 50)];
    assert!(covers(&ranges, 12, 30) && !covers(&ranges, 12, 31) && !covers(&ranges, 5, 15));
    assert!(touches(&ranges, 29, 35) && !touches(&ranges, 30, 40));

    let config = PoolConfig {
        page_size: PageSize::MIN,
        capacity: 256 << 20,
    };
    if !MAPS {
        let pool = BufferPool::new(config).expect("holds frames");
        assert_eq!(pool.mapped_memory(), 0);
        return;
    }
    let linux = cfg!(target_os = "linux");
    let (mut living, mut gone) = (linux.then(Mappings::ready), linux.then(Mappings::ready));
    let before = (!linux).then(address_space);

    let pool = BufferPool::new(config).expect("holds frames");
    let mapped = pool.mapped_memory() as u64;
    assert_eq!(mapped, (pool.frames() * pool.frame_len()) as u64);

    // Where the frames are: they lie side by side from the lowest of them.
    let mut blanks: Vec<Blank<'_>> = (0..pool.frames()).map(|_| pool.blank()).collect();
    assert_eq!(pool.overflow_frames(), 0);
    let start = blanks
        .iter()
        .map(|blank| blank.as_ptr().addr() as u64)
        .min()
        .expect("a frame");
    blanks[0].fill(7);
    drop(blanks);

    if let (Some(living), Some(gone)) = (&mut living, &mut gone) {
        living.read();
        // Nothing is allocated or opened from here to the second reading.
        drop(pool);
        gone.read();
        assert!(
            covers(&living.ranges(), start, start + mapped),
            "the frames are mapped while the pool lives"
        );
        assert!(
            !touches(&gone.ranges(), start, start + mapped),
            "and no part of them once it is dropped"
        );
    } else {
        let before = before.expect("read before the pool was made");
        let with = address_space();
        drop(pool);
        let after = address_space();
        assert!(
            with >= before + mapped,
            "the frames are mapped: {before} bytes, then {with}"
        );
        // Beside its frames the pool allocated its headers and its table,
        // which are far less than an eighth of the frames and which the
        // allocator may keep.
        assert!(
            after <= with - mapped + mapped / 8,
            "and unmapped with the pool: {with} bytes, then {after}"
        );
    }
}
