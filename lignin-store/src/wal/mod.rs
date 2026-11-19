//! The write-ahead log: the files `wal/<n>.log` of a database directory.

mod error;
mod file;
mod reader;
mod store;

#[cfg(test)]
mod tests;

pub use error::LogError;
pub use reader::{FileEnd, LogEnd, LogRead, LogReader, DEFAULT_READ_WINDOW_LEN};
pub use store::{Appended, GroupPlace, LogOptions, LogStore, SyncPolicy, DEFAULT_TARGET_FILE_LEN};
