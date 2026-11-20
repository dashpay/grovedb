//! The frames of a large pool are a mapping of the pool's own on Linux
//! (x86-64 and AArch64) and on macOS: there while the pool lives, and gone
//! when it is dropped.
//!
//! On Linux the test reads the ranges of addresses the process has mapped,
//! and looks for the frames' own range among them. On macOS, which has no
//! such file, it reads the size of the process's address space through
//! `ps`. It is the one test of its file, so that its thread is the only
//! one of the process that maps or allocates while it measures. Miri has
//! neither to read, and does not report a mapping that is never unmapped.

#![cfg(not(miri))]

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

/// What the test reads of the process's memory.
enum Seen {
    /// Linux: the ranges of addresses that are mapped, in their order.
    Ranges(Vec<(u64, u64)>),
    /// macOS: the size of the address space, in bytes.
    Size(u64),
}

fn seen() -> Seen {
    if cfg!(target_os = "linux") {
        let maps = std::fs::read_to_string("/proc/self/maps").expect("the mappings");
        let ranges = maps
            .lines()
            .map(|line| {
                let range = line.split(' ').next().expect("a range");
                let (start, end) = range.split_once('-').expect("two addresses");
                let address = |text| u64::from_str_radix(text, 16).expect("an address");
                (address(start), address(end))
            })
            .collect();
        Seen::Ranges(ranges)
    } else {
        let asked = std::process::Command::new("ps")
            .args(["-o", "vsz=", "-p", &std::process::id().to_string()])
            .output()
            .expect("ps runs");
        let text = String::from_utf8(asked.stdout).expect("text");
        let kib: u64 = text.trim().parse().expect("a number");
        Seen::Size(kib * 1024)
    }
}

/// Whether every address from `start` up to `end` is in one of the ranges.
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

#[test]
fn the_frames_of_a_large_pool_are_mapped_while_it_lives_and_unmapped_with_it() {
    let config = PoolConfig {
        page_size: PageSize::MIN,
        capacity: 256 << 20,
    };
    if !MAPS {
        let pool = BufferPool::new(config).expect("holds frames");
        assert_eq!(pool.mapped_memory(), 0);
        return;
    }
    // The first reading is taken after this thread has allocated, so that
    // what the allocator maps for a new thread is there already.
    let before = seen();
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

    let with = seen();
    drop(pool);
    let after = seen();
    match (before, with, after) {
        (Seen::Ranges(_), Seen::Ranges(with), Seen::Ranges(after)) => {
            assert!(
                covers(&with, start, start + mapped),
                "the frames are mapped while the pool lives"
            );
            assert!(
                !touches(&after, start, start + mapped),
                "and no part of them once it is dropped"
            );
        }
        (Seen::Size(before), Seen::Size(with), Seen::Size(after)) => {
            assert!(
                with >= before + mapped,
                "the frames are mapped: {before} bytes, then {with}"
            );
            // Beside its frames the pool allocated its headers and its
            // table, which are far less than an eighth of the frames and
            // which the allocator may keep.
            assert!(
                after <= with - mapped + mapped / 8,
                "and unmapped with the pool: {with} bytes, then {after}"
            );
        }
        _ => unreachable!("one system, one way to read"),
    }
}

#[test]
fn a_range_is_covered_by_ranges_that_join_and_touched_by_any_that_overlap() {
    let ranges = [(10, 20), (20, 30), (40, 50)];
    assert!(covers(&ranges, 12, 30));
    assert!(!covers(&ranges, 12, 31));
    assert!(!covers(&ranges, 5, 15));
    assert!(touches(&ranges, 29, 35));
    assert!(!touches(&ranges, 30, 40));
}
