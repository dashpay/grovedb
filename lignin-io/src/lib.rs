//! LigninDB file layer: the Vfs trait, the std backend and the fault-injecting backend
//!
//! Everything LigninDB does to a disk goes through [`Vfs`] and [`VfsFile`],
//! whose contract is in the [`vfs`] module. [`StdVfs`] is the backend over
//! the operating system's files.
//!
//! This crate knows files as bytes and nothing above them. A module that
//! needs `unsafe` code has to allow it for itself.

#![deny(unsafe_code)]

#[cfg(test)]
mod conformance;
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub mod std_fs;
pub mod vfs;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub use std_fs::StdVfs;
pub use vfs::{Create, DirEntry, EntryKind, OpenMode, Vfs, VfsFile, VfsLock};
