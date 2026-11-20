//! The frames of a large pool are a mapping of the pool's own on Linux
//! (x86-64 and AArch64) and on macOS, with 64-bit pointers: there while
//! the pool lives, and gone when it is dropped.
//!
//! On Linux the test reads the ranges of addresses the process has mapped,
//! and looks for the frames' own range among them. Between the drop of the
//! pool and the end of the reading that follows, its thread allocates
//! nothing and opens nothing: the file is open and the buffer is there
//! before. On macOS it reads no list of ranges but the size of the
//! process's address space, through `ps`. That is a looser test, in which
//! every other change of the address space counts as well.
//!
//! It is the one test of its file, so that no other test's thread maps or
//! allocates while it measures. Miri reads neither thing, and does not
//! report a mapping that is never unmapped.

#![cfg(not(miri))]

use std::{
    fs::File,
    io::{ErrorKind, Read},
};

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

/// Reads the ranges of addresses out of the text of `/proc/self/maps`,
/// which it is given in pieces of any length. Each line begins with two
/// addresses in hexadecimal, a hyphen between them and a space after the
/// second. The rest of a line can be a path that is not text.
#[derive(Default)]
struct Scan {
    part: Part,
    from: u64,
    to: u64,
}

/// Which part of a line the next byte belongs to.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
enum Part {
    #[default]
    From,
    To,
    Rest,
}

impl Scan {
    /// Takes the next piece of the text, and hands on each range whose
    /// second address ends in it.
    fn take(&mut self, piece: &[u8], mut range: impl FnMut(u64, u64)) {
        for &byte in piece {
            match (self.part, byte) {
                (Part::Rest, b'\n') => *self = Self::default(),
                (Part::Rest, _) => {}
                (Part::From, b'-') => self.part = Part::To,
                (Part::To, b' ') => {
                    self.part = Part::Rest;
                    range(self.from, self.to);
                }
                (Part::From, digit) => self.from = (self.from << 4) | hex(digit),
                (Part::To, digit) => self.to = (self.to << 4) | hex(digit),
            }
        }
    }
}

fn hex(digit: u8) -> u64 {
    let digit = char::from(digit).to_digit(16).expect("an address");
    u64::from(digit)
}

/// A reading of `/proc/self/maps` that is made ready before it is taken:
/// the file is open and the buffer for a piece of its text is there, so
/// that taking it allocates nothing and opens nothing, however long the
/// text is.
struct Reading {
    file: File,
    piece: Vec<u8>,
}

impl Reading {
    fn ready() -> Self {
        Self {
            file: File::open("/proc/self/maps").expect("the mappings"),
            piece: vec![0; 1 << 16],
        }
    }

    /// Reads the file to its end, and hands on each range of it.
    fn take(&mut self, mut range: impl FnMut(u64, u64)) {
        let mut scan = Scan::default();
        loop {
            match self.file.read(&mut self.piece) {
                Ok(0) => break,
                Ok(read) => scan.take(&self.piece[..read], &mut range),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) => panic!("the mappings: {error}"),
            }
        }
        assert_eq!(scan.part, Part::From, "the text ends with a whole line");
    }
}

/// How far the addresses from some start on are covered without a gap,
/// when they were covered up to `reached` and the range that comes next in
/// the order of the addresses is counted in.
fn reach(reached: u64, from: u64, to: u64) -> u64 {
    if from <= reached && reached < to {
        to
    } else {
        reached
    }
}

/// Whether the range has any address from `start` up to `end`.
fn touches(from: u64, to: u64, start: u64, end: u64) -> bool {
    from < end && start < to
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
    // What the two readings are judged by, and what reads them.
    let sample = [(10, 20), (20, 30), (40, 50)];
    let reached = |start| {
        let counted = |reached, &(from, to): &(u64, u64)| reach(reached, from, to);
        sample.iter().fold(start, counted)
    };
    assert_eq!((reached(12), reached(5), reached(35)), (30, 5, 35));
    assert!(touches(20, 30, 29, 35) && !touches(20, 30, 30, 40) && !touches(40, 50, 30, 40));
    let text = b"a-14 r-xp /\xff\n14-1e rw-p\n";
    for cut in 0..=text.len() {
        let (mut scan, mut seen) = (Scan::default(), Vec::new());
        scan.take(&text[..cut], |from, to| seen.push((from, to)));
        scan.take(&text[cut..], |from, to| seen.push((from, to)));
        assert_eq!(seen, [(10, 20), (20, 30)], "cut at {cut}");
    }

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

    if linux {
        let end = start + mapped;
        // Both readings are made ready before the pool is dropped. This
        // thread allocates and opens nothing from the drop to the end of
        // the second reading.
        let mut living = Reading::ready();
        let mut gone = Reading::ready();
        let mut reached = start;
        living.take(|from, to| reached = reach(reached, from, to));
        drop(pool);
        let mut touched = false;
        gone.take(|from, to| touched |= touches(from, to, start, end));
        assert!(reached >= end, "the frames are mapped while the pool lives");
        assert!(!touched, "and no part of them once it is dropped");
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
