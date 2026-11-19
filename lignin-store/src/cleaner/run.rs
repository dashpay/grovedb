//! The background loop of the cleaner.

use std::sync::atomic::Ordering;

use super::{clean::Cleaner, error::CleanerError, state::lock, values::ValuePass};

impl Cleaner<'_> {
    /// The background loop: a pass over the segments ([`Self::pass`]) and
    /// then `values`, again and again, until [`Self::stop`] is called.
    /// Between two turns it sleeps for the idle time of the configuration,
    /// which with the bytes a pass may move is the cleaner's throttle. A
    /// pass that fails is counted, its error is passed to `failed`, and
    /// the loop goes on.
    ///
    /// `values` makes the pass over the value log
    /// ([`Self::pass_values`]). It is the caller's because the owners of
    /// values are: it makes them on this thread, under an operation guard
    /// of its own if they ask for one.
    ///
    /// The store has no thread of its own. Whoever opens it runs this on
    /// one, beside write-back's loop.
    pub fn run(
        &self,
        mut values: impl FnMut(&Self) -> Result<ValuePass, CleanerError>,
        mut failed: impl FnMut(CleanerError),
    ) {
        let state = self.state;
        while !state.stopping.load(Ordering::Acquire) {
            if let Err(error) = self.pass() {
                failed(error);
            }
            if let Err(error) = values(self) {
                failed(error);
            }
            let mut woken = lock(&state.wake);
            if !*woken && !state.stopping.load(Ordering::Acquire) {
                let (next, _) = state
                    .woken
                    .wait_timeout(woken, state.config.idle)
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                woken = next;
            }
            *woken = false;
        }
        state.stopping.store(false, Ordering::Release);
    }

    /// Ends the background loop after its current turn. A loop that has
    /// not begun yet ends as soon as it begins.
    pub fn stop(&self) {
        self.state.stopping.store(true, Ordering::Release);
        self.state.wake();
    }
}
