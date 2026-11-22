//! LigninDB file layer: the Vfs trait, the std backend and the fault-injecting backend
//!
//! Everything LigninDB does to a disk goes through [`Vfs`] and [`VfsFile`],
//! whose contract is in the [`vfs`] module. [`StdVfs`] is the backend over
//! the operating system's files. [`SimVfs`] is the backend in memory that
//! storage code is tested on: it crashes at a chosen operation, gives every
//! state the contract lets the crash leave, and injects `ENOSPC` and `EIO`,
//! all from a seed. The [`sim`] module describes it. On Linux, with the
//! feature `direct_io`, `linux::DirectVfs` is the backend whose reads go
//! past the page cache.
//!
//! This crate knows files as bytes and nothing above them. A module that
//! needs `unsafe` code has to allow it for itself.

#![deny(unsafe_code)]
#![deny(clippy::undocumented_unsafe_blocks)]

#[cfg(test)]
mod conformance;
#[cfg(all(target_os = "linux", feature = "direct_io"))]
pub mod linux;
pub mod sim;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod std_fs;
pub mod vfs;

pub use sim::SimVfs;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use std_fs::StdVfs;
pub use vfs::{Create, DirEntry, EntryKind, OpenMode, SyncItem, Vfs, VfsFile, VfsLock};
