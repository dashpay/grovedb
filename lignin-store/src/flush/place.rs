//! Placing pending values: the value goes into the value log, a manifest
//! covers it, and a page step gives its cell the pointer
//! (`transactions.md` FL7, PV7).

use std::collections::{BTreeMap, HashSet};

use lignin_format::{value_log::PendingValue, CommitVersion, FormatVersion, PageId};

use super::{
    config::count,
    error::FlushError,
    page::{self, PageKind, PendingCell},
    state::{lock, Files, Ready},
    write_back::{Round, Stores, WriteBack},
};
use crate::{
    cache::PageRef,
    values::{CountStep, ValueChange, ValueHold, ValueStoreError},
    wal::LogError,
};

/// What a test does when a page step whose record the log refused has
/// given its locks back.
#[cfg(test)]
pub(super) type AfterARefusedStep = Box<dyn FnOnce()>;

#[cfg(test)]
thread_local! {
    /// What a test has run, once, when a page step whose record the log
    /// refused has given back its lock on the page and the lock on the
    /// counts: what another does to the log from then on.
    pub(super) static AFTER_A_REFUSED_STEP: std::cell::RefCell<Option<AfterARefusedStep>> =
        const { std::cell::RefCell::new(None) };
}

/// What became of one value that was ready for its page step.
enum Step {
    /// The cell holds the pointer, and the value is counted.
    Placed,
    /// The step cannot be made now, and the value stays in its list: no
    /// manifest covers it, or a write transaction holds a private copy of
    /// the page and the value is listed for the committers already.
    PutOff,
    /// A write transaction holds a private copy of the page, and the
    /// value has left the list of the pass for the committers' one.
    HeldBack,
    /// The log refused the record of the step, with what it answered.
    /// Nothing was installed, and the value stays ready.
    Refused(LogError),
    /// The current version of the page has no cell with that pending
    /// value any more: a commit replaced or removed it, carried it to
    /// another page or freed the page. The value is dead space.
    GivenUp,
}

/// Which list a value that is tried for its page step is in.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Listed {
    /// The list of the pass: no private copy was found to hold the value
    /// back yet.
    ForPasses,
    /// The list of the values that a private copy held back.
    ForCommitters,
}

impl<'s> WriteBack<'s> {
    /// Every value that waits for its page step, in whichever list.
    ///
    /// A pass asks, and a pass alone moves a value out of its own list.
    /// A committer moves values between the other two while it holds the
    /// lock of the first of them, under which the keys of both are
    /// copied here: what a committer waits for meanwhile is that copy,
    /// and nothing that grows with the list of the pass.
    fn ready(&self) -> HashSet<(PageId, PendingValue)> {
        let state = self.state;
        let key = |ready: &Ready| (ready.page, ready.cell.pending);
        let held: Vec<_> = {
            let held_back = lock(&state.held_back);
            let found = lock(&state.found_held_back);
            held_back.iter().chain(found.iter()).map(key).collect()
        };
        let mut all: HashSet<_> = lock(&state.placing).iter().map(key).collect();
        all.extend(held);
        all
    }

    /// Appends the pending values of the current page versions in `owed`
    /// to the value log and has a manifest cover them. They are then ready
    /// for their page steps ([`Self::place_ready`]).
    ///
    /// A page whose values cannot be read is left out, with its error
    /// noted in `round`.
    pub(super) fn append_pending(
        &self,
        files: &mut Files,
        owed: &BTreeMap<PageId, Vec<CommitVersion>>,
        round: &mut Round,
    ) -> Result<(), FlushError> {
        let Stores {
            pool, page_values, ..
        } = self.stores;
        let ready = self.ready();
        let mut order = Vec::new();
        let mut fresh = Vec::new();
        let mut ended = Ok(());
        'pages: for (&page, versions) in owed {
            #[cfg(test)]
            super::write_back::PENDING_SCANS.with(|scans| scans.set(scans.get() + 1));
            let Some(frame) = versions
                .last()
                .and_then(|&born| pool.get_version(page, born))
            else {
                continue;
            };
            if !pool.page_step(page).is_current(&frame) {
                continue;
            }
            let mut cells = match page::pending_cells(&frame) {
                Ok(cells) => cells,
                Err(source) => {
                    round.fail(FlushError::Page { page, source });
                    continue;
                }
            };
            if let Some(page_values) = page_values
                && cells.len() > 1
                && page::kind_of(&frame) == PageKind::Tree
            {
                // In the order a scan reads them, so that the values of a
                // page lie in the value log as they will be read.
                order.clear();
                page_values.scan_order(&frame, &mut order);
                let place = |cell: &PendingCell| {
                    let found = order.iter().position(|&slot| Some(slot) == cell.slot);
                    found.unwrap_or(usize::MAX)
                };
                cells.sort_by_key(place);
            }
            for cell in cells {
                if ready.contains(&(page, cell.pending)) {
                    continue;
                }
                match self.append_value(files, page, &frame, cell) {
                    Ok(appended) => fresh.push(appended),
                    Err(error) if error.is_of_one_page() => {
                        round.fail(error);
                        continue 'pages;
                    }
                    Err(error) => {
                        ended = Err(error);
                        break 'pages;
                    }
                }
            }
        }

        // What was appended is covered whatever ended the walk, and what
        // ended the walk is returned whatever becomes of the cover.
        let covered = if fresh.is_empty() {
            Ok(())
        } else {
            self.cover(files, fresh)
        };
        ended.and(covered)
    }

    /// Makes the values in `fresh`, which a pass appended to the value
    /// log, durable under a manifest that covers them, and has them wait
    /// for their page steps.
    ///
    /// Whatever became of the sync, and of the manifest where the sync
    /// did not fail, the value log is asked of each value last
    /// ([`ValueStore::covers`](crate::values::ValueStore::covers)). A
    /// value that it then knows a manifest to cover waits for its page
    /// step, and every other one is given up with its hold
    /// ([`FlushStats::values_given_up`](super::FlushStats::values_given_up)).
    /// A pass can fill one file and go on in the next: the values of the
    /// first are covered by the manifest that named the second, whatever
    /// becomes of the sync of the second.
    fn cover(&self, files: &mut Files, fresh: Vec<Ready>) -> Result<(), FlushError> {
        let values = self.stores.values;
        let synced = files.values.as_mut().map(|writer| writer.sync());
        let covered = match synced {
            Some(Err(error)) => {
                // What that sync was for cannot be made durable in the
                // file any more. The file is closed where it was durable,
                // and is not synced again.
                self.give_up_value_file(files);
                Err(error.into())
            }
            _ => self
                .manifest()
                .and_then(|mut manifest| Ok(values.commit(&mut manifest)?)),
        };
        let mut placing = lock(&self.state.placing);
        for appended in fresh {
            if values.covers(appended.pointer) {
                placing.push(appended);
            } else {
                count(&self.state.counters.values_given_up, 1);
            }
        }
        covered
    }

    /// Appends the bytes of one pending value to the value log, for the
    /// page that owns it, under a hold of its own.
    fn append_value(
        &self,
        files: &mut Files,
        page: PageId,
        frame: &PageRef<'s>,
        cell: PendingCell,
    ) -> Result<Ready, FlushError> {
        let log = self.stores.log;
        let born = frame.born();
        let len = cell.pending.value_len() as usize;
        files.value.clear();
        match frame.attached(cell.pending.id()) {
            Some(bytes) => files.value.extend_from_slice(bytes),
            None => {
                // The frame does not have the bytes, so the log does: the
                // pending value names where (`log-format.md` LG21).
                let mut record = Vec::new();
                let logged = log.read_pending(&cell.pending, page, &mut record)?;
                let Some(bytes) = logged.bytes() else {
                    return Err(FlushError::PendingValue { page, born });
                };
                bytes
                    .parts()
                    .for_each(|part| files.value.extend_from_slice(part));
            }
        }
        if files.value.len() != len {
            return Err(FlushError::PendingValue { page, born });
        }

        let writer = self.value_writer(&mut files.values)?;
        let mut hold = ValueHold::new();
        let pointer = match writer.append(&mut hold, page, &files.value) {
            Ok(pointer) => pointer,
            Err(error) => {
                if !matches!(error, ValueStoreError::Format { .. }) {
                    // The values that went into the file before this one
                    // are made durable there, and the manifest of this
                    // pass covers them ([`Self::cover`]).
                    self.give_up_value_file(files);
                }
                return Err(error.into());
            }
        };
        let counters = &self.state.counters;
        count(&counters.values_appended, 1);
        count(&counters.value_bytes, len as u64);
        count(
            &counters.value_stored_bytes,
            pointer.extent_len(FormatVersion::CURRENT),
        );
        Ok(Ready {
            page,
            cell,
            pointer,
            _hold: hold,
        })
    }

    /// Goes through the list of the values that a private copy of their
    /// page held back from their step, and tries the page step of each
    /// one it comes to, as a pass tries any value: the step is made where
    /// the page has no private copy now, a manifest covers the value and
    /// the log takes the record. It ends at the first step that fails
    /// with an error, and has not tried the values after that one.
    /// Returns how many values were placed.
    ///
    /// **Whoever commits calls this once its commit is published** (after
    /// step 8 of CM3) and before the next write transaction of its thread
    /// copies a page, and so does a transaction that has rolled back:
    /// that is the moment at which the copies that held a step back are
    /// gone. A pass that finds a private copy of a value's page cannot
    /// place the value, and where blocks are applied back to back a pass
    /// can find one every time it looks: these calls come when the
    /// copies of their caller are gone, and so can place such a value.
    ///
    /// **The call tries the values of that list and no others.** It
    /// never takes the lock of the pass's own list, so it does not wait
    /// for a pass to have gone through the pass's values. For each value
    /// it tries it takes what any page step takes.
    ///
    /// **A value that a pass found held back by a copy is listed for
    /// these calls before that copy is gone, and stays listed until its
    /// step is made or it is given up**, by a call or by a pass. The
    /// pass lists the value under the lock of the value's page, under
    /// which it saw the copy and under which a copy is installed or
    /// discarded.
    ///
    /// It touches no file but the log, to which it appends one record for
    /// each value it places. It syncs nothing, but where the log took an
    /// append back: the log is then settled, which can sync it.
    ///
    /// A step whose record the log answers with
    /// [`LogError::NotAppended`], with [`LogError::NoActiveFile`], or
    /// with [`LogError::TakenBack`] where the log is settled after it,
    /// is put off like one that a private copy holds back, and this call
    /// returns no error for it: the refusal has asked the loop for a
    /// turn, for the cut the log needs, and a pass in which a record is
    /// refused so ends with an error ([`Self::pass`]). A value stays in
    /// the list it was in when the log refused its record.
    pub fn place_ready(&self) -> Result<usize, FlushError> {
        let mut placed = 0;
        self.place_held_back(&mut placed, &mut None)?;
        Ok(placed)
    }

    /// The placing steps of a pass: the values that a private copy held
    /// back are tried first, as [`Self::place_ready`] tries them, and then
    /// every other value that is ready. `placed` counts the values that
    /// were placed, also where the call ends with an error, and `refused`
    /// is given what the log answered the first record that it refused.
    /// Such a step is made again by a later call, so the pass has failed
    /// by it, and the loop waits before its next turn.
    pub(super) fn place_each(
        &self,
        placed: &mut usize,
        refused: &mut Option<LogError>,
    ) -> Result<(), FlushError> {
        let mut placing = lock(&self.state.placing);
        self.place_held_back(placed, refused)?;
        self.place_all(&mut placing, Listed::ForPasses, placed, refused)
    }

    /// Tries the values that a private copy held back, with those that a
    /// pass has found so since the last call.
    fn place_held_back(
        &self,
        placed: &mut usize,
        refused: &mut Option<LogError>,
    ) -> Result<(), FlushError> {
        let mut held_back = lock(&self.state.held_back);
        held_back.append(&mut lock(&self.state.found_held_back));
        self.place_all(&mut held_back, Listed::ForCommitters, placed, refused)
    }

    /// Tries the values of `list` one after the other, each at most
    /// once, and ends at the first step that fails with an error.
    fn place_all(
        &self,
        list: &mut Vec<Ready>,
        listed: Listed,
        placed: &mut usize,
        refused: &mut Option<LogError>,
    ) -> Result<(), FlushError> {
        let counters = &self.state.counters;
        let mut at = 0;
        while at < list.len() {
            match self.place(list, at, listed)? {
                Step::Placed => {
                    // The hold goes now that the value is counted.
                    drop(list.swap_remove(at));
                    count(&counters.values_placed, 1);
                    *placed += 1;
                }
                Step::GivenUp => {
                    drop(list.swap_remove(at));
                    count(&counters.values_given_up, 1);
                }
                // The value has left the list.
                Step::HeldBack => {}
                Step::PutOff => at += 1,
                Step::Refused(error) => {
                    refused.get_or_insert(error);
                    at += 1;
                }
            }
        }
        Ok(())
    }

    /// The page step that places the value `list[at]` (`transactions.md`
    /// PV7, with its three checks).
    ///
    /// A value of the pass's list whose page a write transaction holds a
    /// private copy of is moved out of `list` here, under the lock of the
    /// page: the copy is installed or discarded under that lock, so
    /// whoever looks for held-back values once the copy is gone finds
    /// this one.
    fn place(&self, list: &mut Vec<Ready>, at: usize, listed: Listed) -> Result<Step, FlushError> {
        let Stores {
            pool,
            values,
            versions,
            ..
        } = self.stores;
        let ready = &list[at];
        let (page, pointer, pending) = (ready.page, ready.pointer, ready.cell.pending.id());
        let Ok(change) = page::placing(page, &ready.cell, pointer) else {
            return Ok(Step::GivenUp);
        };
        let mut record = Vec::new();
        if change.write(&mut record).is_err() {
            return Ok(Step::GivenUp);
        }

        // The counts lock comes before the page's (LT7, level 4a). What
        // the value log says of the pointer is asked before the page's
        // lock is taken, under which only the pool is called.
        let counts = values.step()?;
        let covered = values.covers(pointer);
        let step = pool.page_step(page);
        let Some(old) = step.current() else {
            return Ok(Step::GivenUp);
        };
        if !page::is_due(&old, &change).unwrap_or(false) {
            return Ok(Step::GivenUp);
        }
        if !covered {
            count(&self.state.counters.steps_put_off, 1);
            return Ok(Step::PutOff);
        }
        if step.private_copies() > 0 {
            count(&self.state.counters.steps_put_off, 1);
            if listed == Listed::ForCommitters {
                return Ok(Step::PutOff);
            }
            let held_back = list.swap_remove(at);
            lock(&self.state.found_held_back).push(held_back);
            return Ok(Step::HeldBack);
        }
        let mut blank = pool.blank();
        blank.copy_from(&old);
        if page::apply_step(&mut blank, self.size, &change).is_err() {
            return Ok(Step::GivenUp);
        }
        blank.detach(pending);

        if let Err(error) = self.append_change(&record) {
            let cause = self.cause_of(&counts, error);
            drop(blank);
            drop(step);
            drop(counts);
            #[cfg(test)]
            if let Some(then) = AFTER_A_REFUSED_STEP.with(|hook| hook.borrow_mut().take()) {
                then();
            }
            return self.record_not_written(cause).map(Step::Refused);
        }
        step.replace(&old, blank)
            .map_err(|source| FlushError::StepRefused { page, source })?;
        drop(step);
        let mut acquired = ValueChange::new();
        acquired.acquire(pointer);
        // The change is applied whatever this returns: an error says that
        // a count was found wrong, and the file is then kept.
        let _ = counts.apply(&acquired, versions.latest());
        Ok(Step::Placed)
    }

    /// What the log answered a page step whose record it did not take,
    /// with the cause of it where the log had no file that takes items:
    /// what left the log so, if the log has kept it
    /// ([`LogStore::refusal`](crate::wal::LogStore::refusal)).
    ///
    /// The step asks while it holds the lock on the counts, whose guard
    /// it shows as `held`: under that lock no turn gives the log a file
    /// and no append of another fails, so what the log has kept then is
    /// what kept this record out.
    fn cause_of(&self, held: &CountStep<'_>, error: LogError) -> LogError {
        let _ = held;
        match error {
            LogError::NoActiveFile => self.stores.log.refusal().unwrap_or(error),
            error => error,
        }
    }

    /// What a page step does when the append of its record failed, with
    /// its locks given back. Nothing was installed. `error` is what the
    /// log answered, with its cause ([`Self::cause_of`]).
    ///
    /// - The record is not in the log, and the log takes none until it has
    ///   been rolled: the value stays ready for a later call. If the
    ///   append was taken back, a crash could still bring the record
    ///   back, so the log is settled first (`wal`, "When the file layer
    ///   fails"), and a settling that fails is the error of the call.
    ///   Otherwise what the log answered is returned as the refusal of
    ///   the step.
    /// - The log cannot find out what became of the append: the record may
    ///   be in the log, so the value keeps its hold for as long as the
    ///   session lasts, and the log takes nothing more in it. That is the
    ///   caller's to know, and is returned as the error of the call.
    fn record_not_written(&self, error: LogError) -> Result<LogError, FlushError> {
        match error {
            LogError::TakenBack { .. } => {
                self.stores.log.settle()?;
                Ok(error)
            }
            LogError::NotAppended { .. }
            | LogError::NoActiveFile
            | LogError::NoActiveFileSince { .. } => Ok(error),
            error => Err(error.into()),
        }
    }
}
