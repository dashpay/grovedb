//! The background loop of write-back, and the backpressure it puts on
//! whoever commits (`transactions.md` FL5).

use std::{
    sync::atomic::Ordering,
    time::{Duration, Instant},
};

use super::{
    checkpoint::Checkpointed,
    config::count,
    error::FlushError,
    state::lock,
    write_back::{Pass, Stores, WriteBack},
};

/// What a committer that asked for room was told
/// ([`WriteBack::wait_for_room`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Room {
    /// Write-back is within its bounds. The commit goes on.
    Free,
    /// Write-back is over its bounds, and a turn that was made while the
    /// caller waited moved nothing on: what is left waits for something that
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
    /// The checkpoint that was taken, if one was due and nothing stood in
    /// its way.
    pub checkpoint: Option<Checkpointed>,
}

impl Tick {
    /// Whether the turn moved anything on: its pass did, or a table
    /// checkpoint was installed.
    pub fn progressed(&self) -> bool {
        self.pass.progressed() || matches!(self.checkpoint, Some(Checkpointed::Installed { .. }))
    }
}

impl WriteBack<'_> {
    /// Whether write-back is within its bounds: no more page versions wait
    /// to be written back, no more bytes of pending values are held in
    /// frames, and no more log is held ([`Self::log_held`]), than the
    /// configuration allows.
    pub fn has_room(&self) -> bool {
        let config = &self.state.config;
        self.state.owed.load(Ordering::Relaxed) <= config.max_unwritten
            && self.stores.pool.attached_bytes() <= config.max_pending_bytes
            && self.log_held() <= config.max_log_held
    }

    /// Backpressure: returns at once while write-back is within its
    /// bounds, and otherwise once it is again, or once a turn of the loop
    /// has shown that waiting longer would not help.
    ///
    /// A committer calls this **between commits and never while it holds
    /// the commit lock**, so backpressure stops commits only between
    /// commits, and no reader is slowed by it (FL5). There are two places
    /// for the call, and they differ in what the wait can achieve:
    ///
    /// - **When the caller holds no private copy**: once its commit is
    ///   published and [`Self::place_ready`] has returned, before its next
    ///   write transaction copies a page. Everything the caller committed
    ///   can be placed and written while it waits, and a table checkpoint
    ///   can cover it. This is the call that keeps the log within its
    ///   bound when blocks are applied back to back.
    /// - **Before step 2 of CM3**, with the copies of the transaction that
    ///   is about to commit. What those copies hold back cannot be written
    ///   while the caller waits, so the wait ends when a turn has moved
    ///   nothing on ([`Room::Stalled`]): no committer waits for write-back
    ///   that its own copies hold back.
    ///
    /// With a background loop running ([`Self::run`]), the caller waits for
    /// its turns. With none, the caller makes the turns itself
    /// ([`Self::tick`]), and their errors are its own.
    pub fn wait_for_room(&self) -> Result<Room, FlushError> {
        if self.has_room() {
            return Ok(Room::Free);
        }
        let began = Instant::now();
        let room = self.wait_while_full();
        let counters = &self.state.counters;
        count(&counters.room_waits, 1);
        if matches!(room, Ok(Room::Stalled)) {
            count(&counters.room_stalls, 1);
        }
        let waited = u64::try_from(began.elapsed().as_nanos()).unwrap_or(u64::MAX);
        count(&counters.room_waited_nanos, waited);
        room
    }

    fn wait_while_full(&self) -> Result<Room, FlushError> {
        loop {
            if self.has_room() {
                return Ok(Room::Free);
            }
            let progressed = match self.state.running.load(Ordering::Acquire) {
                true => match self.wait_for_turn() {
                    Some(progressed) => progressed,
                    // The loop has ended: the caller makes the turn.
                    None => continue,
                },
                false => self.tick()?.progressed(),
            };
            if !progressed {
                return Ok(match self.has_room() {
                    true => Room::Free,
                    false => Room::Stalled,
                });
            }
        }
    }

    /// Waits for a turn of the background loop that begins from now on, so
    /// that it has seen everything the caller committed, and returns
    /// whether it moved anything on. `None` if the loop ended first.
    fn wait_for_turn(&self) -> Option<bool> {
        let state = self.state;
        let mut turns = lock(&state.turns);
        let begun = turns.begun;
        state.wake();
        while turns.ended <= begun {
            if !state.running.load(Ordering::Acquire) {
                return None;
            }
            let (next, _) = state
                .turned
                .wait_timeout(turns, state.config.idle)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            turns = next;
        }
        Some(turns.progress)
    }

    /// One turn of the background loop: the log is rolled and synced if
    /// that is due, a pass is made, and a table checkpoint is taken if one
    /// is due.
    ///
    /// The pass is made whatever became of the log, so that a committer
    /// that waits for room is never left waiting. No checkpoint is tried
    /// in a turn in which something failed, or whose pass found a page
    /// that a write transaction holds back ([`Pass::blocked`]): it would
    /// be put off. The first error of the turn is returned.
    pub fn tick(&self) -> Result<Tick, FlushError> {
        self.counted_turn().1
    }

    /// [`Self::tick`], with whether the turn moved anything on, which it
    /// can have done though it failed.
    fn counted_turn(&self) -> (bool, Result<Tick, FlushError>) {
        let state = self.state;
        let number = {
            let mut turns = lock(&state.turns);
            turns.begun += 1;
            turns.begun
        };
        let (progress, turn) = self.turn();
        let mut turns = lock(&state.turns);
        turns.ended = number;
        turns.progress = progress;
        drop(turns);
        state.turned.notify_all();
        (progress, turn)
    }

    /// One turn of the background loop and what follows it there: a turn
    /// that failed is counted and passed to `failed`. Returns how long the
    /// loop waits before its next turn, whoever asks for one, or `None`
    /// after a turn that did not fail. `in_vain` is the loop's count of
    /// the turns in a row that failed and moved nothing on.
    pub(super) fn paced_turn(
        &self,
        in_vain: &mut u32,
        failed: &mut dyn FnMut(FlushError),
    ) -> Option<Duration> {
        let state = self.state;
        let (progressed, turn) = self.counted_turn();
        match turn {
            Ok(_) => {
                *in_vain = 0;
                None
            }
            Err(error) => {
                *in_vain = match progressed {
                    true => 0,
                    false => in_vain.saturating_add(1),
                };
                count(&state.counters.errors, 1);
                failed(error);
                Some(state.config.wait_after_failure(*in_vain))
            }
        }
    }

    /// One turn, and whether it moved anything on, which it can have done
    /// though it failed.
    fn turn(&self) -> (bool, Result<Tick, FlushError>) {
        let rolled = self.roll_log();
        let synced = self.sync_log_if_due();
        let (pass, passed) = self.pass_through();
        let turn = rolled.and_then(|rolled| {
            synced?;
            passed?;
            let checkpoint = match self.checkpoint_due() && pass.blocked == 0 {
                true => Some(self.checkpoint()?),
                false => None,
            };
            Ok(Tick {
                rolled,
                pass,
                checkpoint,
            })
        });
        let progress = match &turn {
            Ok(tick) => tick.progressed(),
            Err(_) => pass.progressed(),
        };
        (progress, turn)
    }

    /// The background loop: [`Self::tick`], again and again, until
    /// [`Self::stop`] is called. Between two turns it sleeps until it is
    /// woken ([`Self::wake`]) or the idle time of the configuration has
    /// passed. A turn that fails is counted ([`FlushStats::errors`]), its
    /// error is passed to `failed`, and the loop goes on: what it could not
    /// do stays owed, and nothing committed is lost.
    ///
    /// **A turn that failed does not bring the next turn forward.** After
    /// it the loop waits for as long as
    /// [`FlushConfig::wait_after_failure`] says, whoever asks for a turn
    /// before that wait or during it: the idle time after the first turn
    /// that failed and moved nothing on, and twice as long after each
    /// further such turn in a row, up to
    /// [`FlushConfig::max_failed_wait`], or up to the idle time where
    /// that bound is the shorter. An idle time of less than a
    /// millisecond counts as a millisecond here. After a failed turn
    /// that moved something on the wait is the idle time and the row
    /// begins anew, and a turn that does not fail is followed by the
    /// sleep of the first paragraph. A wait in which a turn was asked
    /// for is counted ([`FlushStats::turns_put_off`]). `failed` is
    /// called before the wait begins. Only [`Self::stop`] and
    /// [`Self::room_made`] end the wait before its time.
    ///
    /// **A failed turn has moved something on** if its pass did what no
    /// later turn has to do again ([`Pass::progressed`]): it wrote a page
    /// version to a segment, gave a value its place, or kept a superseded
    /// version for its readers and took it off what is owed. A roll of
    /// the log and a file that was made do not count, since a turn can
    /// roll and make files in vain, and neither does a value that was
    /// appended and has no place yet.
    ///
    /// The store has no thread of its own. Whoever opens it runs this on
    /// one, for as long as the stores live.
    ///
    /// [`FlushStats::errors`]: super::FlushStats::errors
    /// [`FlushStats::turns_put_off`]: super::FlushStats::turns_put_off
    /// [`FlushConfig::wait_after_failure`]: super::FlushConfig::wait_after_failure
    /// [`FlushConfig::max_failed_wait`]: super::FlushConfig::max_failed_wait
    pub fn run(&self, mut failed: impl FnMut(FlushError)) {
        let state = self.state;
        state.running.store(true, Ordering::Release);
        let mut in_vain = 0;
        while !state.stopping.load(Ordering::Acquire) {
            let failed_wait = self.paced_turn(&mut in_vain, &mut failed);
            state.sleep(failed_wait);
        }
        state.running.store(false, Ordering::Release);
        state.stopping.store(false, Ordering::Release);
        state.turned.notify_all();
    }

    /// Wakes the background loop for a turn: after a commit was published,
    /// say, so that its pages are written while they are still in the
    /// processor's cache. A loop whose last turn failed finishes its wait
    /// first ([`Self::run`]).
    pub fn wake(&self) {
        self.state.wake();
    }

    /// Tells the background loop that room was made on the device, by
    /// whoever made it: a session that gave back a reserve of bytes, say.
    /// The sleep that the loop is in, or the one that follows the turn it
    /// is in, ends at once, also where it is the wait after a turn that
    /// failed ([`Self::run`]): the turn after it is made on a device with
    /// more room. No later sleep is ended by the call, and the loop's
    /// count of failed turns in a row is as it was.
    pub fn room_made(&self) {
        self.state.room_made();
    }

    /// Ends the background loop after its current turn. A loop that has
    /// not begun yet ends as soon as it begins.
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
    ///
    /// The files are closed and the log is made durable whatever became of
    /// the checkpoint, whose error comes first. **Every step of the close
    /// is made whatever became of the one before it**, and the first error
    /// is returned: each of the three stores replaces the manifest with one
    /// that names its files as they are, also when the store before it
    /// could not delete a file, and also when a manifest in doubt was
    /// settled with such an answer. Only a manifest that is still in doubt,
    /// because it could not be opened again, is replaced by none of them.
    ///
    /// A replacement of the close that fails can leave an earlier
    /// manifest in force, as a session that is killed does: one that
    /// names the value log's file as one that is still written, and the
    /// log's last file open and short of its end. The next session's
    /// recovery takes a value-log file at the length its manifest gives
    /// it, which covers every value a page points to, since a value is
    /// covered by a manifest before a page is given its pointer. It reads
    /// a log file that is named open past the named length, up to the
    /// first item that is not whole, and lands on the last commit whose
    /// group lies whole before that item.
    pub fn finish(&self) -> Result<Checkpointed, FlushError> {
        let checkpointed = self.checkpoint();
        let closed = self.close();
        let checkpointed = checkpointed?;
        closed?;
        Ok(checkpointed)
    }

    fn close(&self) -> Result<(), FlushError> {
        let mut files = lock(&self.state.files);
        let sealed = match files.segment.take() {
            Some(writer) => self.seal(&mut files, writer),
            None => Ok(()),
        };
        let finished = match files.values.take() {
            Some(writer) => self.finish_value_file(writer),
            None => Ok(()),
        };
        drop(files);
        let synced = self.sync_log().map(drop);
        // A manifest in doubt is settled first. A store that answers the
        // settling with an error has heard of the manifest all the same,
        // so the stores name their files in the manifest as it is: a
        // replacement refuses one that is still in doubt, and writes
        // nothing.
        let settled = self.manifest().map(drop);
        let named = {
            let mut manifest = lock(self.stores.manifest);
            let Stores {
                segments,
                values,
                log,
                ..
            } = self.stores;
            let segments = segments.commit(&mut manifest).map_err(FlushError::from);
            let values = values.commit(&mut manifest).map_err(FlushError::from);
            let log = log.commit(&mut manifest).map_err(FlushError::from);
            segments.and(values).and(log)
        };
        sealed.and(finished).and(synced).and(settled).and(named)
    }
}
