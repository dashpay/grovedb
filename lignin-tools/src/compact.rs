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
//! 2. it lets the cleaner work towards no dead byte in the segments and in
//!    the value log, with a table checkpoint for every file it empties,
//!    until a whole pass over the segments and a whole pass over the value
//!    log have begun and ended since the cleaner last moved, trimmed or
//!    dropped anything, or until the timeout;
//! 3. it closes the database, and prints the files on disk before and
//!    after, with what the cleaner did.
//!
//! **Exit status.** A pass of the cleaner that failed, or other background
//! work that failed, ends the work when the next look at the counts sees
//! it, between two rounds of the reclaimer or two polls. The report is
//! written from the counts taken before the close, and the exit status is
//! 1 where those counts hold a failure or the close failed (the close's
//! error is reported after the counts). The counts are taken once, before
//! the close: a pass that fails while the close waits for it to end is
//! not in them. A timeout is no failure: the report says that the work
//! was cut.
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
    thread,
    time::{Duration, Instant},
};

use lignin_store::cleaner::{CleanerConfig, CleanerStats};
use lignindb::{db::DbStats, Db};

use crate::{
    args::Common,
    open::{open_with, refuse_parts},
    stats::{files_on_disk, write_files, OnDisk},
    Outcome, ToolError,
};

/// How often the counts are looked at while the cleaner works.
const POLL: Duration = Duration::from_millis(50);

/// The cleaner's policy for a compaction: no dead byte left that a move
/// can take, a table checkpoint as soon as any segment waits for one, and
/// no rest between passes.
pub fn policy() -> CleanerConfig {
    CleanerConfig {
        target_dead_percent: 0,
        checkpoint_dead_percent: 0,
        value_dead_percent: 0,
        idle: Duration::from_millis(10),
        ..CleanerConfig::new()
    }
}

/// What tells one moment of the cleaner's work from the next.
fn progress(stats: &CleanerStats) -> [u64; 6] {
    [
        stats.blocks_moved,
        stats.links_trimmed,
        stats.segments_dropped,
        stats.values_moved,
        stats.value_files_dropped,
        stats.checkpoints,
    ]
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
    // The counts before the close decide, whatever the polls saw: a pass
    // can fail after the last of them.
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

/// Reclaims, then waits until a pass over the segments and a pass over
/// the value log have each begun and ended since the cleaner last changed
/// anything, until the background work fails, or until `deadline`.
///
/// A pass is counted when it begins: of two that began since the last
/// change, the first has ended, and found nothing to do.
fn settle(db: &Db, deadline: Instant) -> Ended {
    let stats = loop {
        let more = db.reclaim().more;
        let stats = db.stats();
        if failed(&stats) {
            return Ended::Failed;
        }
        if !more || Instant::now() >= deadline {
            break stats;
        }
    };
    let mut last = progress(&stats.cleaner);
    let mut since = (stats.cleaner.passes, stats.cleaner.value_passes);
    while Instant::now() < deadline {
        thread::sleep(POLL);
        let stats = db.stats();
        if failed(&stats) {
            return Ended::Failed;
        }
        let now = progress(&stats.cleaner);
        let passes = (stats.cleaner.passes, stats.cleaner.value_passes);
        if now != last {
            last = now;
            since = passes;
        } else if passes.0 >= since.0 + 2 && passes.1 >= since.1 + 2 {
            return Ended::Quiet;
        }
    }
    Ended::Timeout
}
