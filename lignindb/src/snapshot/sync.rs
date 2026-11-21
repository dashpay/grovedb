//! The atomics the pins and guards are built from.
//!
//! A normal build takes them from `std`. A build with `--cfg loom` takes them
//! from `loom`, which runs the code of this module under the interleavings of
//! a model.

#[cfg(loom)]
pub(super) use loom::sync::atomic::{AtomicBool, AtomicU64, Ordering};
#[cfg(not(loom))]
pub(super) use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

/// An atomic that holds `value`.
///
/// Every access in this module is `SeqCst`. Loom gives a new atomic its
/// first value as a weaker store, which a `SeqCst` load could then still
/// return after a later `SeqCst` store. So under loom the value is stored
/// once more, the way every later value is.
pub(super) fn atomic(value: u64) -> AtomicU64 {
    let atomic = AtomicU64::new(value);
    #[cfg(loom)]
    atomic.store(value, Ordering::SeqCst);
    atomic
}

/// A flag that is not set. See [`atomic`] for why it is stored once more
/// under loom.
pub(super) fn flag() -> AtomicBool {
    let flag = AtomicBool::new(false);
    #[cfg(loom)]
    flag.store(false, Ordering::SeqCst);
    flag
}

/// A value on cache lines of its own, so that a thread that writes it does
/// not slow the threads that use its neighbours.
#[derive(Debug, Default)]
#[repr(align(128))]
pub(super) struct Padded<T>(pub(super) T);
