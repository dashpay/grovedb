//! A recorded history replayed on the two engines.
//!
//! The fuzzer's streams are generated. This replays a stream that was
//! recorded: the calls a caller of `GroveDb` made on it, written down by a
//! recorder inside an earlier revision of GroveDB as each call returned,
//! with the caller's marks of its requests and of the blocks it finalized
//! ([`stream`]).
//!
//! [`replay`] makes the calls on a fresh database of the reference engine
//! and of the candidate, in the order of the stream and through the
//! transactions of the stream, and holds the two answers of each call
//! against each other: the outcome (a value by its `Debug` text, an error
//! by the name two engines are compared by), the cost, and what the
//! callbacks of a batch were asked. After every block it holds the two
//! root hashes against each other. At the end the candidate's store is
//! read by LigninDB's physical checker, as at the end of a case of the
//! fuzzer ([`super::close`]).
//!
//! What it does not hold alike it says, and it does not call it alike:
//!
//! - A line that is no whole record, a method it does not know, a write it
//!   cannot make, an argument it does not read, a call made inside a
//!   callback of another call, and the calls of a second thread each end
//!   the replay. The stream has the order in which calls returned, which
//!   for the last two is not the order of what they did.
//! - A read in [`calls::READS_NOT_MADE`] is not made, and is counted.
//! - A call for which the stream lacks an argument is made with a stand-in
//!   and counted apart: a write of an aux value too long for the recorder
//!   (other bytes of its length), and the two reads that make delete
//!   operations (none of the operations their caller was building).
//!
//! It is not [`super::pair::run`]: a case of the pair runner is a list of
//! the test kit's own operations on one grove version, applied with the
//! test kit's options and callbacks. A recorded call has its caller's
//! options, the answers its caller's callbacks gave, and its own grove
//! version, and is made with those ([`calls`]).
//!
//! The reference's answers are also held against the recording node's,
//! which the stream has: where they differ, the revision of this workspace
//! answers a call otherwise than the recorded one did. That is counted by
//! method, with what was not held against the recording, and is no
//! divergence of the engines.
//!
//! ```text
//! cargo run --release -p lignin-testkit --bin lignin-history -- \
//!     STREAM --scratch DIR [--stop-height N] [--show-heights A-B,C] \
//!     [--watch FILE] [--keep-going N] [--progress N]
//! ```

pub mod calls;
pub mod replay;
pub mod stream;

pub use self::replay::{replay, report, Settings, Summary};
