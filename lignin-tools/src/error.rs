//! Every way a subcommand fails.

use std::{error::Error, fmt, io, path::PathBuf};

use lignin_format::FormatError;
use lignin_store::{load::LoadError, segment::SegmentStoreError};
use lignindb::{
    db::{DbError, OpenError, WriteError},
    snapshot::ReadError,
    txn::TxnError,
};

/// Why a subcommand did not do what it was asked.
#[derive(Debug)]
pub enum ToolError {
    /// The arguments are not a subcommand.
    Usage(String),
    /// The directory is no database the tool reads: a part of a
    /// checkpoint, or a database that is being removed.
    NotADatabase {
        /// The directory.
        dir: PathBuf,
        /// What it is.
        what: &'static str,
    },
    /// The database did not open.
    Open(OpenError),
    /// The manifest or a file it names could not be read.
    Files(SegmentStoreError),
    /// A file the manifest names is not what the manifest says.
    Format {
        /// The file.
        path: PathBuf,
        /// Where in it.
        offset: u64,
        /// What is wrong.
        source: FormatError,
    },
    /// A page could not be read.
    Page(LoadError),
    /// A record, a node or an entry could not be read.
    Read(ReadError),
    /// A write transaction refused a write.
    Txn(TxnError),
    /// A commit failed.
    Write(WriteError),
    /// The close or the flush of the database failed.
    Db(DbError),
    /// A dump is not one this build reads, or does not hold what `load`
    /// needs.
    Dump(String),
    /// The background work of `compact` failed: its report is written,
    /// and the error is the last one.
    Background(String),
    /// The file layer failed: writing the report, or a dump.
    Io(io::Error),
}

impl ToolError {
    /// The exit status of the process.
    pub fn status(&self) -> i32 {
        match self {
            Self::Usage(_) => 2,
            _ => 1,
        }
    }
}

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Usage(what) => f.write_str(what),
            Self::NotADatabase { dir, what } => {
                write!(f, "{} is no database: {what}", dir.display())
            }
            Self::Open(error) => write!(f, "the database did not open: {error}"),
            Self::Files(error) => write!(f, "{error}"),
            Self::Format {
                path,
                offset,
                source,
            } => write!(f, "{} at byte {offset}: {source}", path.display()),
            Self::Page(error) => write!(f, "a page: {error}"),
            Self::Read(error) => write!(f, "a read: {error}"),
            Self::Txn(error) => write!(f, "a write: {error}"),
            Self::Write(error) => write!(f, "the commit: {error}"),
            Self::Db(error) => write!(f, "{error}"),
            Self::Dump(what) => write!(f, "the dump: {what}"),
            Self::Background(what) => write!(f, "the background work failed: {what}"),
            Self::Io(error) => write!(f, "{error}"),
        }
    }
}

impl Error for ToolError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Usage(_) | Self::Dump(_) | Self::Background(_) | Self::NotADatabase { .. } => {
                None
            }
            Self::Open(error) => Some(error),
            Self::Files(error) => Some(error),
            Self::Format { source, .. } => Some(source),
            Self::Page(error) => Some(error),
            Self::Read(error) => Some(error),
            Self::Txn(error) => Some(error),
            Self::Write(error) => Some(error),
            Self::Db(error) => Some(error),
            Self::Io(error) => Some(error),
        }
    }
}

macro_rules! from {
    ($($variant:ident($error:ty)),* $(,)?) => {$(
        impl From<$error> for ToolError {
            fn from(error: $error) -> Self {
                Self::$variant(error)
            }
        }
    )*};
}

from! {
    Open(OpenError),
    Files(SegmentStoreError),
    Page(LoadError),
    Read(ReadError),
    Txn(TxnError),
    Write(WriteError),
    Db(DbError),
    Io(io::Error),
}
