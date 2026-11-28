//! The background loop of the cleaner.

use std::{sync::atomic::Ordering, time::Duration};

use super::{clean::Cleaner, error::CleanerError, values::ValuePass};
use crate::flush::FlushConfig;

impl Cleaner<'_> {
    /// The background loop: a pass over the segments ([`Self::pass`]) and
    /// then `values`, again and again, until [`Self::stop`] is called.
    /// Between two turns it sleeps for the idle time of the configuration,
    /// which with the bytes a pass may move is the cleaner's throttle. A
    /// pass that fails is counted, its error is passed to `failed`, and
    /// the loop goes on.
    ///
    /// **A turn that failed does not bring the next turn forward**, as in
    /// write-back's loop ([`WriteBack::run`](crate::flush::WriteBack::run)):
    /// after a turn in which a pass failed the loop waits its idle time,
    /// and twice as long after each further such turn in a row, up to
    /// [`FlushConfig::max_failed_wait`] of the store's write-back, whoever
    /// wakes it. The wait is the idle time again after the first turn in
    /// which no pass fails. Only [`Self::stop`] and [`Self::room_made`]
    /// end it before that.
    ///
    /// `values` makes the pass over the value log
    /// ([`Self::pass_values`]). It is the caller's because the owners of
    /// values are: it makes them on this thread. It takes no operation
    /// guard for them, since the pass takes the one they are used under.
    ///
    /// The store has no thread of its own. Whoever opens it runs this on
    /// one, beside write-back's loop.
    pub fn run(
        &self,
        mut values: impl FnMut(&Self) -> Result<ValuePass, CleanerError>,
        mut failed: impl FnMut(CleanerError),
    ) {
        let state = self.state;
        // The turns in a row, up to the last one, in which a pass failed.
        let mut failed_turns = 0_u32;
        while !state.stopping.load(Ordering::Acquire) {
            let failed_wait = self.paced_turn(&mut values, &mut failed, &mut failed_turns);
            state.sleep(failed_wait);
        }
        state.stopping.store(false, Ordering::Release);
    }

    /// One turn of the background loop: the two passes, each made
    /// whatever became of the other. What is returned is how long the
    /// loop waits before its next turn, whoever wakes it, or `None` after
    /// a turn in which no pass failed. `failed_turns` is the loop's count
    /// of the turns in a row in which one did.
    pub(super) fn paced_turn(
        &self,
        values: &mut dyn FnMut(&Self) -> Result<ValuePass, CleanerError>,
        failed: &mut dyn FnMut(CleanerError),
        failed_turns: &mut u32,
    ) -> Option<Duration> {
        let mut turn_failed = false;
        if let Err(error) = self.pass() {
            turn_failed = true;
            failed(error);
        }
        if let Err(error) = values(self) {
            turn_failed = true;
            failed(error);
        }
        *failed_turns = match turn_failed {
            true => failed_turns.saturating_add(1),
            false => 0,
        };
        let pacing = FlushConfig {
            idle: self.state.config.idle,
            ..self.write_back.state().config()
        };
        turn_failed.then(|| pacing.wait_after_failure(*failed_turns))
    }

    /// Tells the background loop that room was made on the device, by
    /// whoever made it: a session that gave back a reserve of bytes, say.
    /// The sleep that the loop is in, or the one that follows the turn it
    /// is in, ends at once, also where it is the wait after a turn in
    /// which a pass failed ([`Self::run`]): the turn after it is made on a
    /// device with more room. No later sleep is ended by the call, and
    /// the loop's count of failed turns in a row is as it was.
    pub fn room_made(&self) {
        self.state.room_made();
    }

    /// Ends the background loop after its current turn. A loop that has
    /// not begun yet ends as soon as it begins.
    pub fn stop(&self) {
        self.state.stopping.store(true, Ordering::Release);
        self.state.wake();
    }
}
