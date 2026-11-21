//! When what a drop left may be reclaimed.

use lignin_format::CommitVersion;
use lignin_store::alloc::{Epoch, Pins};

/// The wait between a drop and the reclaiming of what it left
/// (`transactions.md` PN6, CL6): until no version pin is below the drop's
/// version, and until the operations that were running then have ended.
///
/// The drop's version is not kept anywhere. Whoever reads the record of a
/// drop reads it at some version, and the drop was committed by then: that
/// version stands for the drop's, and makes the wait no shorter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wait {
    retired: CommitVersion,
    epoch: Option<Epoch>,
}

impl Wait {
    /// The wait for what a drop left, whose record was read at the version
    /// `seen`: the latest version, read under the guard of the operation
    /// that read the record.
    pub const fn new(seen: CommitVersion) -> Self {
        Self {
            retired: seen,
            epoch: None,
        }
    }

    /// The version the wait counts the drop from.
    pub const fn seen(&self) -> CommitVersion {
        self.retired
    }

    /// Asks the three questions of PN6, in their order, and says whether
    /// the wait is over. It never waits: the caller asks again later.
    ///
    /// The caller holds no operation guard across two calls: a guard that
    /// was taken before the call that found no pin below the drop is one
    /// the wait is for.
    pub fn poll(&mut self, pins: &dyn Pins) -> bool {
        if self.epoch.is_none() {
            if pins.oldest_pin() < self.retired {
                return false;
            }
            self.epoch = Some(pins.next_epoch());
        }
        self.epoch.is_some_and(|epoch| epoch <= pins.oldest_guard())
    }
}
