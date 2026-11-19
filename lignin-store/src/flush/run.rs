//! The background loop of write-back, and the backpressure it puts on
//! whoever commits (`transactions.md` FL5).

use std::sync::atomic::Ordering;

use super::{
    checkpoint::Checkpointed,
    config::count,
    error::FlushError,
    state::lock,
    write_back::{Pass, WriteBack},
};

/// What a committer that asked for room was told
/// ([`WriteBack::wait_for_room`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Room {
    /// Write-back is within its bounds. The commit goes on.
    Free,
    /// Write-back is over its bounds, and a pass that ran while the caller
    /// waited moved nothing on: what is left waits for something that
    /// cannot happen while the caller waits, such as the end of a write
    /// transaction that holds a private copy. The commit goes on.
    Stalled,
}

/// What one turn of the background loop did ([`WriteBack::tick`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tick {
    /// Whether the log turned to another file.
    pub rolled: bool,
    /// What the pass did.
    pub pass: Pass,
    /// The checkpoint that was taken, if one was due.
    pub checkpoint: Option<Checkpointed>,
}

impl WriteBack<'_> {
    /// Whether write-back is within its bounds: no more page versions wait
    /// to be written back, and no more bytes of pending values are held in
    /// frames, than the configuration allows.
    pub fn has_room(&self) -> bool {
        let config = &self.state.config;
        self.state.owed.load(Ordering::Relaxed) <= config.max_unwritten
            && self.stores.pool.attached_bytes() <= config.max_pending_bytes
    }

    /// Backpressure: returns at once while write-back is within its
    /// bounds, and otherwise once it is again, or once a pass has shown
    /// that waiting longer would not help.
    ///
    /// A committer calls this **before it takes the commit lock** (before
    /// step 2 of CM3), and never while it holds it: so backpressure stops
    /// commits only between commits, and no reader is slowed by it. What
    /// the caller's own private copies hold back cannot be written while it
    /// waits, so the wait ends when a pass has moved nothing on
    /// ([`Room::Stalled`]): no committer waits for write-back that its own
    /// copies hold back (FL5).
    ///
    /// With a background loop running ([`Self::run`]), the caller waits for
    /// its passes. With none, the caller makes the passes itself.
    pub fn wait_for_room(&self) -> Result<Room, FlushError> {
        loop {
            if self.has_room() {
                return Ok(Room::Free);
            }
            let progressed = if self.state.running.load(Ordering::Acquire) {
                let mut room = lock(&self.state.room);
                let seen = room.passes;
                self.state.wake();
                while room.passes == seen && self.state.running.load(Ordering::Acquire) {
                    let (next, _) = self
                        .state
                        .room_changed
                        .wait_timeout(room, self.state.config.idle)
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    room = next;
                }
                room.passes != seen && room.progress
            } else {
                self.pass()?.progressed()
            };
            if !progressed {
                return Ok(match self.has_room() {
                    true => Room::Free,
                    false => Room::Stalled,
                });
            }
        }
    }

    /// One turn of the background loop: the log is rolled and synced if
    /// that is due, a pass is made, and a table checkpoint is taken if one
    /// is due.
    pub fn tick(&self) -> Result<Tick, FlushError> {
        let rolled = self.roll_log()?;
        self.sync_log_if_due()?;
        let pass = self.pass()?;
        let checkpoint = match self.checkpoint_due() {
            true => Some(self.checkpoint()?),
            false => None,
        };
        Ok(Tick {
            rolled,
            pass,
            checkpoint,
        })
    }

    /// The background loop: [`Self::tick`], again and again, until
    /// [`Self::stop`] is called. Between two turns it sleeps until it is
    /// woken ([`Self::wake`]) or the idle time of the configuration has
    /// passed. A turn that fails is counted ([`FlushStats::errors`]), its
    /// error is passed to `failed`, and the loop goes on: what it could not
    /// do stays owed, and nothing committed is lost.
    ///
    /// The store has no thread of its own. Whoever opens it runs this on
    /// one, for as long as the stores live.
    ///
    /// [`FlushStats::errors`]: super::FlushStats::errors
    pub fn run(&self, mut failed: impl FnMut(FlushError)) {
        let state = self.state;
        state.stopping.store(false, Ordering::Release);
        state.running.store(true, Ordering::Release);
        while !state.stopping.load(Ordering::Acquire) {
            if let Err(error) = self.tick() {
                count(&state.counters.errors, 1);
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
        state.running.store(false, Ordering::Release);
        state.room_changed.notify_all();
    }

    /// Wakes the background loop for a turn: after a commit was published,
    /// say, so that its pages are written while they are still in the
    /// processor's cache.
    pub fn wake(&self) {
        self.state.wake();
    }

    /// Ends the background loop after its current turn.
    pub fn stop(&self) {
        self.state.stopping.store(true, Ordering::Release);
        self.state.wake();
    }

    /// What a session does before it ends: writes back what it can, takes
    /// a table checkpoint, seals the segment and closes the value-log file
    /// it was filling, and makes the log durable with a manifest that says
    /// so, so that the next session has no log file to write again.
    ///
    /// The background loop is stopped first ([`Self::stop`]) and has
    /// returned. A page version that still cannot be written, because a
    /// write transaction is open, stays in the log.
    pub fn finish(&self) -> Result<Checkpointed, FlushError> {
        let checkpointed = self.checkpoint()?;
        let mut files = lock(&self.state.files);
        if let Some(mut writer) = files.segment.take()
            && let Err(error) = writer.seal()
        {
            self.segment_failed(&mut files, writer);
            return Err(error.into());
        }
        if let Some(writer) = files.values.take() {
            writer.finish()?;
        }
        drop(files);
        self.sync_log()?;
        let Self { stores, .. } = self;
        let mut manifest = self.manifest()?;
        stores.segments.commit(&mut manifest)?;
        stores.values.commit(&mut manifest)?;
        stores.log.commit(&mut manifest)?;
        Ok(checkpointed)
    }
}
