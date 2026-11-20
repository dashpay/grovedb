//! The frames of a large pool are a mapping of the pool's own on Linux
//! (x86-64 and AArch64) and on macOS: there while the pool lives, and gone
//! when it is dropped.
//!
//! The test reads the size of the process's address space from the system:
//! on Linux in a file, on macOS through `ps`. It is the one test of its
//! file, so that its thread is the only one of the process that maps or
//! allocates while it measures. Miri has no such size to read, and does
//! not report a mapping that is never unmapped.

#![cfg(not(miri))]

use lignin_format::PageSize;
use lignin_store::cache::{BufferPool, PoolConfig};

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

/// The address space the process has mapped, in bytes.
fn address_space() -> u64 {
    let kib: u64 = if cfg!(target_os = "linux") {
        let status = std::fs::read_to_string("/proc/self/status").expect("the status");
        let line = status
            .lines()
            .find_map(|line| line.strip_prefix("VmSize:"))
            .expect("the size of the address space");
        let kib = line.trim().strip_suffix("kB").expect("in kB");
        kib.trim().parse().expect("a number")
    } else {
        let asked = std::process::Command::new("ps")
            .args(["-o", "vsz=", "-p", &std::process::id().to_string()])
            .output()
            .expect("ps runs");
        let text = String::from_utf8(asked.stdout).expect("text");
        text.trim().parse().expect("a number")
    };
    kib * 1024
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
    let before = address_space();
    let pool = BufferPool::new(config).expect("holds frames");
    let mapped = pool.mapped_memory() as u64;
    assert_eq!(mapped, (pool.frames() * pool.frame_len()) as u64);
    let with = address_space();
    assert!(
        with >= before + mapped,
        "the frames are mapped: {before} bytes, then {with}"
    );

    let mut blank = pool.blank();
    blank.fill(7);
    drop(blank);
    drop(pool);

    // Beside its frames the pool allocated its headers and its table,
    // which are far less than an eighth of the frames and which the
    // allocator may keep.
    let after = address_space();
    assert!(
        after <= with - mapped + mapped / 8,
        "and unmapped with the pool: {with} bytes, then {after}"
    );
}
