//! The golden trace corpus: what the RocksDB engine answers, written down.
//!
//! The corpus is the definition of "same answers" for LigninDB. For every
//! grove version and every workload it records, step by step, what GroveDB
//! on the RocksDB engine returned at a pinned commit:
//!
//! - the `OperationCost` of each operation;
//! - the root hash after each write, batch and commit;
//! - the result of each read;
//! - for each read GroveDB proves, the proof bytes, the cost of proving and
//!   whether the proof verifies.
//!
//! A replay runs the same operations again and compares. It is green when
//! every one of those values is the same, on any engine and at any commit.
//!
//! # What is recorded
//!
//! [`plan`] lists the traces. A trace is one database: the operation streams
//! applied to it, in order, from empty. Its file names the trace, the seed,
//! the grove version and, for each stream, the workload, so every recorded
//! step is tied to a workload, a seed and a grove version. The streams come
//! from [`workload`](crate::workload) and are applied through
//! [`workload::apply`](crate::workload::apply()), the same way every other
//! consumer applies them. [`legacy`] adds the behaviour of older grove
//! versions that the generators steer around, written by hand.
//!
//! # How it is stored
//!
//! The files are text ([`trace`]) under `lignin-testkit/corpus`. Every step
//! has one short line: a 16-bit digest of each part, enough to say which
//! part of which step differs. A sample of steps, and every step of the
//! hand-written cases, is written out in full. One 256-bit digest per trace
//! covers every byte of every step and decides whether a replay is the same.
//! The full traces are not committed: [`corpus::dump`] regenerates them.
//!
//! # Using it
//!
//! ```text
//! cargo test -p lignin-testkit --test golden              replay, as CI does
//! cargo run --release -p lignin-testkit --bin lignin-golden -- replay
//! cargo run --release -p lignin-testkit --bin lignin-golden -- record
//! cargo run --release -p lignin-testkit --bin lignin-golden -- dump --out DIR
//! ```
//!
//! `record` regenerates the whole standard corpus for whatever
//! [`Workload::suite`](crate::workload::Workload::suite) returns.
//! `--reference` selects the million-document scale. When a replay differs,
//! `dump` the trace from this checkout and from the commit the trace names,
//! and compare the two files.
//!
//! A consumer that computes with the steps, as a cost model checked against
//! every recorded `OperationCost` does, takes them as values from
//! [`record::each_step`] over the traces of [`plan::plan`].
//!
//! # Extending it
//!
//! A new workload in the suite needs no change here: record again. A new
//! kind of write is applied by `workload::apply` and recorded as it is. Two
//! places name every case of a workload type, without a fallback, so the
//! compiler stops at them when the type grows:
//!
//! - [`Runner`](run::Runner)'s `value` turns what `apply` returned, an
//!   [`Outcome`](crate::workload::Outcome), into the value that is
//!   compared, and its `run` does the same for each shape of answer
//!   `GroveDb::run_path_query` has. A new outcome or shape is one more
//!   [`Value::Record`](step::Value::Record): a name and its fields. The
//!   encoding and the long form of a record are written once.
//! - [`run::provable`] says which [`Read`](crate::workload::Read) is proved
//!   and with what. A new kind of read that GroveDB can prove has to be
//!   proved, or its proof bytes are compared by nothing.
//!
//! A value that is already recorded keeps its encoding: change one and
//! every trace holding it has to be recorded again, the reference scale
//! included.
//!
//! Another engine is opened in one place, `record`'s `fresh_database`.

pub mod cli;
pub mod corpus;
pub mod legacy;
pub mod plan;
pub mod record;
pub mod run;
pub mod step;
pub mod trace;

use std::{
    fmt,
    path::{Path, PathBuf},
};

/// What can stop a recording, a replay or a dump. A replay that differs
/// from the corpus is not an error: it is the replay's report.
#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// The engine failed where a trace has nowhere to record a failure.
    Engine {
        /// What was being done.
        what: String,
        /// The engine's message.
        message: String,
    },
    /// A file or directory could not be read or written.
    Io {
        /// The file or directory.
        path: PathBuf,
        /// The system's message.
        message: String,
    },
    /// The engine commit could not be read from git.
    Git(String),
}

impl Error {
    pub(crate) fn engine(what: &str, error: &impl fmt::Display) -> Self {
        Error::Engine {
            what: what.to_string(),
            message: error.to_string(),
        }
    }

    pub(crate) fn io(path: &Path, error: &std::io::Error) -> Self {
        Error::Io {
            path: path.to_path_buf(),
            message: error.to_string(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Engine { what, message } => write!(f, "the engine failed {what}: {message}"),
            Error::Io { path, message } => write!(f, "{}: {message}", path.display()),
            Error::Git(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for Error {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn errors_say_what_failed() {
        let engine = Error::engine("reading the root hash", &"corrupted");
        assert_eq!(
            engine.to_string(),
            "the engine failed reading the root hash: corrupted"
        );
        let io = Error::io(
            Path::new("/corpus/v1/documents.trace"),
            &std::io::Error::other("denied"),
        );
        assert_eq!(io.to_string(), "/corpus/v1/documents.trace: denied");
        assert_eq!(
            Error::Git("git failed".to_string()).to_string(),
            "git failed"
        );
        let _: &dyn std::error::Error = &engine;
    }
}
