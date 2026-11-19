//! The differential fuzzer: random operation streams applied to two
//! engines, compared after every step.
//!
//! "Same answers" is the first design rule of LigninDB: on every existing
//! grove version, root hash, proof bytes and `OperationCost` are those of
//! the RocksDB engine. The golden corpus ([`crate::golden`]) holds that
//! for the streams that were recorded. The fuzzer holds it for streams
//! nobody wrote down: it generates them, applies each to a database of the
//! reference engine and to one of the candidate, and stops at the first
//! step the two answer differently.
//!
//! # What is compared
//!
//! After every operation ([`pair`]), through the corpus's own
//! [`Runner`](crate::golden::run::Runner), so a step is here what it is
//! there:
//!
//! - the `OperationCost`, also of an operation that fails;
//! - the result, or the name of the error;
//! - the root hash, inside the open transaction;
//! - for a read GroveDB proves: the proof bytes, the cost of proving, and
//!   whether the proof verifies to the committed root;
//! - whether an engine panicked, and with what message.
//!
//! And every so many operations, the records the two databases store
//! ([`state`]): the nodes of every Merk as Merk encodes them, the records
//! of the append-only trees, the aux and meta records, and the drops that
//! are pending, staged or committed. That is where the referrer list of an
//! element that takes backward references shows, which is in no hash and
//! which no read returns, and a record of the auxiliary keyspace, which
//! was never in one. A subtree that is gone since the last comparison is
//! looked into once more, for what a delete, a move or a rollback left
//! behind.
//!
//! # The streams
//!
//! A [`Case`] is a function of a seed ([`case`]). Its operations come from
//! the workload generators, so the fuzzer gains an operation the day a
//! workload produces it, and it nests no deeper than they do
//! ([`case::MAX_DEPTH`]). The grove version, the workloads, how their
//! blocks are interleaved and how the stream is damaged are drawn from the
//! seed. A stream as generated applies without a failure. A damaged one
//! asks for what an engine refuses, and both have to refuse it alike.
//!
//! # The engines
//!
//! [`engines`] names the two: [`Reference`] and [`Candidate`]. Until
//! LigninDB exists the candidate is [`StandIn`], RocksDB behind an `Engine`
//! of its own whose reads are pinned as LigninDB's will be, and in which a
//! defect can be planted. Naming another candidate is one line there.
//!
//! # A divergence
//!
//! The run stops, says what differs at which step, and shrinks the stream
//! to the operations the divergence needs ([`shrink`]). The seed of the
//! case reproduces it.
//!
//! # Using it
//!
//! ```text
//! cargo run --release -p lignin-testkit --bin lignin-diff -- fuzz --duration 3600
//! cargo run --release -p lignin-testkit --bin lignin-diff -- seeded
//! cargo run --release -p lignin-testkit --bin lignin-diff -- case --seed 0x…
//! cargo test -p lignin-testkit --test diff
//! ```
//!
//! `fuzz` compares the reference with the candidate. `seeded` is the
//! fuzzer's own acceptance test: it plants a one-byte cost defect in the
//! stand-in and fails unless the fuzzer finds it within a minute.
//! `tests/diff.rs` runs both for a short while.
//!
//! # What it does not see
//!
//! - What the corpus does not see either: `GroveDb::clear_subtree` and
//!   `GroveDb::fetch_chunk` return no cost, and an error is compared by
//!   the name of its variant, not by its message.
//! - Two engines that panic at the same step with the same message count
//!   as the same answer, and so do two that cannot be observed in the same
//!   way. The run goes on to the next case, and its report says how many
//!   cases were cut short and why the first one was.
//! - What a subtree that was dropped flat still holds while the drop is
//!   pending: an engine may keep those records until the flush or hide
//!   them at the drop. After the flush the subtree is looked into like any
//!   other that is gone. And storage under no path that a comparison ever
//!   reached: the fuzzer reads what GroveDB can address, not what a file
//!   holds.
//! - One transaction writes at a time: the streams are applied from one
//!   thread. Commits that race, and what a crash leaves, are other tasks'
//!   (VER-11, VER-04).
//! - Whether the answers are right. Both engines can be wrong together:
//!   that is what the corpus, with its pinned engine, is for.

pub mod case;
pub mod cli;
pub mod engines;
pub mod fuzz;
pub mod pair;
pub mod shrink;
pub mod state;

pub use self::{
    case::{Case, Shape},
    engines::{Candidate, Fault, Reference, StandIn},
    fuzz::{fuzz, Finding, Settings, Summary},
    pair::{run, Divergence, Part, Run, Side, Stored, Tally},
    shrink::{shrink, Budget, Shrunk},
};
