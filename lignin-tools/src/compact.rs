//! `lignin compact DIR [--timeout SECS]`: reclaim and clean until a
//! whole pass of the cleaner moves nothing.
//!
//! LigninDB has no compaction of sorted runs: dead space is what a later
//! version of a page leaves of an earlier one in a segment, and the value
//! a later write replaced in the value log. The cleaner moves what is live
//! out of files that are mostly dead and gives the files back, at the pace
//! its policy sets (`lignin_store::cleaner`). `compact` opens the database
//! with a policy that leaves no dead byte the cleaner can move:
//!
//! 1. it takes what dropped subtrees left ([`Db::reclaim`]) until a round
//!    of the reclaimer says that no more is to be done now. What waits for
//!    a reader, a write transaction or a later commit stays, and the
//!    report counts it (reclaim jobs waiting, range deletes among them):
//!    no commit of `compact` lets it go on;
//! 2. it makes the cleaner's turns itself ([`Db::clean`]): the cleaner's
//!    thread makes no turn in a session that is asked for no commit, and
//!    `compact` asks for none. Each turn is a pass over the segments and
//!    one over the value log, towards no dead byte in either. A segment
//!    that a pass emptied and that the table checkpoint in force still
//!    points into has a table checkpoint taken before it is removed. The
//!    turns go on until neither pass of one has moved anything on
//!    (`moved_on` in this module says what that is), or until the
//!    timeout, which is looked at between two turns;
//! 3. it closes the database, and prints the files on disk before and
//!    after, with what the cleaner did.
//!
//! **Exit status.** A pass of the cleaner that failed, or other background
//! work that failed, ends the work when the next look at the counts sees
//! it, between two rounds of the reclaimer or two turns of the cleaner.
//! The report is written from the counts taken before the close, and the
//! exit status is 1 where those counts hold a failure or the close failed
//! (the close's error is reported after the counts). A timeout is no
//! failure: the report says that the work was cut.
//!
//! What stays: the segment and the value-log file the session was
//! writing, which no cleaner moves while they are written; blocks and
//! values that a file holds next to dead ones where the cleaner has
//! nothing more to gain by moving them (each file is below the share of
//! the store that [`CleanerConfig::max_segment_percent`] sets, or below
//! [`CleanerConfig::min_segment_len`]); and what the close writes: the
//! pages the session still held changed, whose earlier blocks are dead
//! space that a later `compact` takes.

use std::{
    io::Write,
    time::{Duration, Instant},
};

use lignin_store::cleaner::{CleanerConfig, Pass, ValuePass};
use lignindb::{db::DbStats, Db};

use crate::{
    args::Common,
    open::{open_with, refuse_parts},
    stats::{files_on_disk, write_files, OnDisk},
    Outcome, ToolError,
};

/// The cleaner's policy for a compaction: no dead byte left that a move
/// can take, and a table checkpoint as soon as any segment waits for one.
pub fn policy() -> CleanerConfig {
    CleanerConfig {
        target_dead_percent: 0,
        checkpoint_dead_percent: 0,
        value_dead_percent: 0,
        ..CleanerConfig::new()
    }
}

/// Whether a turn of the cleaner moved anything on, by what its two
/// passes answered: the pass over the segments trimmed, moved, retired,
/// dropped or sealed something, had a table checkpoint installed, or was
/// ended by its throttle with more to move, or the pass over the value
/// log moved a value or dropped a file. A turn that only sealed the
/// segment that was being filled has moved something on: the next turn
/// is the first that can move that segment's live blocks.
fn moved_on(segments: &Pass, values: &ValuePass) -> bool {
    segments.progressed() || segments.more || values.progressed()
}

/// Compacts the database in `common.dir`, for at most `timeout`, and
/// writes what it did to `out`.
pub fn run(common: &Common, timeout: Duration, out: &mut dyn Write) -> Result<Outcome, ToolError> {
    let began = Instant::now();
    let deadline = (began.checked_add(timeout))
        .ok_or_else(|| ToolError::Usage(format!("--timeout {} is too long", timeout.as_secs())))?;
    // Nothing of a directory the open refuses is read.
    refuse_parts(&common.dir)?;
    let before = files_on_disk(&common.dir);
    let db = open_with(common, Some(policy()))?;
    write!(out, "before: ")?;
    write_files(&before, out)?;
    let ended = settle(&db, deadline);
    let stats = db.stats();
    let closed = db.close();
    let cleaner = &stats.cleaner;
    writeln!(
        out,
        "cleaner: {} passes over the segments and {} over the value log; {} blocks moved ({} \
         bytes), {} links trimmed, {} segments dropped ({} bytes), {} values moved ({} bytes), \
         {} value-log files dropped, {} table checkpoints; {} errors",
        cleaner.passes,
        cleaner.value_passes,
        cleaner.blocks_moved,
        cleaner.bytes_moved,
        cleaner.links_trimmed,
        cleaner.segments_dropped,
        cleaner.bytes_dropped,
        cleaner.values_moved,
        cleaner.value_bytes_copied,
        cleaner.value_files_dropped,
        cleaner.checkpoints,
        cleaner.errors
    )?;
    writeln!(
        out,
        "reclaimed: {} dropped subtrees ({} reclaim jobs waiting), {} pages freed",
        stats.reclaimed.merks, stats.reclaimed.waiting, stats.reclaimed.pages_freed
    )?;
    let after = files_on_disk(&common.dir);
    write!(out, "after: ")?;
    write_files(&after, out)?;
    writeln!(
        out,
        "{} bytes on disk before, {} after; {:.1} s",
        total(&before),
        total(&after),
        began.elapsed().as_secs_f64()
    )?;
    if ended == Ended::Timeout {
        writeln!(
            out,
            "the timeout ended the work before a whole pass of the cleaner moved nothing"
        )?;
    }
    closed?;
    // The counts before the close decide.
    if failed(&stats) {
        let error = stats.background.last_error.as_deref();
        return Err(ToolError::Background(error.map_or_else(
            || format!("{} passes of the cleaner failed", cleaner.errors),
            str::to_owned,
        )));
    }
    Ok(Outcome::Done)
}

/// Whether `stats` hold a failure of the cleaner or of other background
/// work.
fn failed(stats: &DbStats) -> bool {
    stats.cleaner.errors > 0 || stats.background.last_error.is_some()
}

/// How the work of [`settle`] ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Ended {
    /// A whole pass of each kind moved nothing.
    Quiet,
    /// The deadline came first.
    Timeout,
    /// A pass of the cleaner, or other background work, failed.
    Failed,
}

/// The bytes of `files`, or `unknown` where a directory was not read.
fn total(files: &OnDisk) -> String {
    let bytes = files.iter().map(|(_, files)| files.map(|(_, bytes)| bytes));
    bytes
        .sum::<Option<u64>>()
        .map_or_else(|| "unknown".to_owned(), |bytes| bytes.to_string())
}

/// Reclaims, then makes turns of the cleaner until one has moved nothing
/// on ([`moved_on`]), until a pass or other background work fails, or
/// until `deadline`.
///
/// A turn is a pass over the segments and one over the value log
/// ([`Db::clean`]).
fn settle(db: &Db, deadline: Instant) -> Ended {
    loop {
        let more = db.reclaim().more;
        if failed(&db.stats()) {
            return Ended::Failed;
        }
        if !more || Instant::now() >= deadline {
            break;
        }
    }
    while Instant::now() < deadline {
        let turn = db.clean();
        if failed(&db.stats()) {
            return Ended::Failed;
        }
        match turn {
            Ok((segments, values)) if moved_on(&segments, &values) => {}
            Ok(_) => return Ended::Quiet,
            Err(_) => return Ended::Failed,
        }
    }
    Ended::Timeout
}

#[cfg(test)]
mod tests {
    use lignin_format::FileId;

    use super::*;

    /// A turn that only sealed the segment that was being filled is not
    /// the last: none of the cleaner's counts of moved, trimmed and
    /// dropped things has changed, and the next turn can move the live
    /// blocks of that segment. So is one that the throttle ended, and one
    /// whose pass over the value log dropped a file. A turn whose passes
    /// answer that nothing was done is the last.
    #[test]
    fn a_turn_that_only_sealed_a_segment_is_not_the_last() {
        let nothing = (Pass::default(), ValuePass::default());
        assert!(!moved_on(&nothing.0, &nothing.1));
        let sealed = Pass {
            sealed: vec![FileId::MIN],
            ..Pass::default()
        };
        assert!(moved_on(&sealed, &nothing.1));
        let throttled = Pass {
            more: true,
            ..Pass::default()
        };
        assert!(moved_on(&throttled, &nothing.1));
        let dropped = ValuePass {
            dropped: vec![FileId::MIN],
            ..ValuePass::default()
        };
        assert!(moved_on(&nothing.0, &dropped));
    }
}
