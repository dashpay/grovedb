//! The version at which chains may be trimmed, for write-back's trims and
//! for the cleaner's: one rule, kept in one place.

use std::collections::VecDeque;

use lignin_format::CommitVersion;

use crate::alloc::{Epoch, Pins};

/// How many answers about the oldest pinned version wait for their
/// operations at once. An answer that has no room is not kept: a later
/// call asks again.
pub(crate) const MAX_BOUNDS: usize = 8;

/// The version at which chains may be trimmed: one that no version pin
/// and no running operation reads below.
///
/// The page table asks for the first alone (`page-table.md` T11), and the
/// oldest pin alone is not that version. An operation that holds no pin
/// reads the latest version of its beginning under its guard, and a link
/// it has not looked up yet is no location it has read
/// (`transactions.md` RD4, PN5): were the chain of a page that a commit
/// superseded or freed meanwhile trimmed at once, the operation would
/// find no version of the page. So the three questions are put as they
/// are for a file (PN6): the oldest pin, then an epoch, and the answer is
/// used once every operation guard of before that epoch is released. An
/// operation that begins later reads a version at or after the answer.
#[derive(Debug, Default)]
pub(crate) struct TrimBound {
    /// Answers about the oldest pinned version, each with the epoch that
    /// was started once it was given: no version pin is below it, and it
    /// waits for the operations that held a guard when that epoch began.
    waiting: VecDeque<(Epoch, CommitVersion)>,
    /// The highest of those answers whose operations have ended: neither
    /// a version pin nor a running operation reads below it.
    at: Option<CommitVersion>,
}

impl TrimBound {
    /// Asks `versions` the three questions and returns the version at
    /// which chains may be trimmed now, or `None` while no such version
    /// is known.
    pub(crate) fn ask<P: Pins + ?Sized>(&mut self, versions: &P) -> Option<CommitVersion> {
        let oldest = versions.oldest_pin();
        let asked = self.waiting.back().map(|(_, at)| *at).or(self.at);
        // A later answer can be lower than an earlier one, and both are
        // true: only an answer that allows more is worth an epoch.
        if asked.is_none_or(|asked| oldest > asked) && self.waiting.len() < MAX_BOUNDS {
            self.waiting.push_back((versions.next_epoch(), oldest));
        }
        let oldest_guard = versions.oldest_guard();
        while let Some((_, at)) = self
            .waiting
            .pop_front_if(|(epoch, _)| *epoch <= oldest_guard)
        {
            self.at = self.at.max(Some(at));
        }
        self.at
    }

    /// How many answers wait for their operations.
    pub(crate) fn waiting(&self) -> usize {
        self.waiting.len()
    }
}
