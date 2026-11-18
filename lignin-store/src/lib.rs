//! LigninDB page store: buffer pool, write-ahead log, segments, page table, recovery and cleaner

#![deny(unsafe_code)]

pub mod alloc;
pub mod segment;
pub mod table;
