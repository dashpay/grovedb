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
    values::{ValueChange, ValueHold, ValueStoreError},
    wal::LogError,
};

/// What became of one value that was ready for its page step.
enum Step {
    /// The cell holds the pointer, and the value is counted.
    Placed,
    /// A write transaction holds a private copy of the page, or no
    /// manifest covers the value. The value stays ready.
    PutOff,
    /// The log refused the record of the step, with what it answered.
    /// Nothing was installed, and the value stays ready.
    Refused(LogError),
    /// The current version of the page has no cell with that pending
    /// value any more: a commit replaced or removed it, carried it to
    /// another page or freed the page. The value is dead space.
    GivenUp,
}

impl<'s> WriteBack<'s> {
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
        let ready: HashSet<(PageId, PendingValue)> = lock(&self.state.placing)
            .iter()
            .map(|ready| (ready.page, ready.cell.pending))
            .collect();
        let mut order = Vec::new();
        let mut fresh = Vec::new();
        let mut ended = Ok(());
        'pages: for (&page, versions) in owed {
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
    /// Whatever becomes of the sync and of the manifest, a value that the
    /// manifest in force covers waits for its page step, and every other
    /// one is given up with its hold, for a later pass to append again
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

    /// Makes the page step of every value that is ready for it and whose
    /// page no write transaction holds a private copy of. Returns how many
    /// values were placed.
    ///
    /// A pass calls this. **Whoever commits calls it as well, once its
    /// commit is published** (after step 8 of CM3) and before the next
    /// write transaction of its thread copies a page, and so does a
    /// transaction that has rolled back: that is the moment at which the
    /// copies that held a step back are gone. A value on a page that every
    /// block changes is placed there and nowhere else, when blocks are
    /// applied back to back.
    ///
    /// It touches no file but the log, to which it appends one record for
    /// each value it places, with no sync. One call runs at a time.
    ///
    /// A step whose record the log refused is put off like one that a
    /// private copy holds back, and is no error of this call: the log
    /// has woken the loop for the cut it needs, and the pass of that
    /// turn is told of a record that it is refused
    /// ([`Self::place_each`]).
    pub fn place_ready(&self) -> Result<usize, FlushError> {
        let mut placed = 0;
        self.place_each(&mut placed, &mut None)?;
        Ok(placed)
    }

    /// [`Self::place_ready`] for a pass. `placed` counts the values that
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
        let counters = &self.state.counters;
        let mut at = 0;
        while at < placing.len() {
            match self.place(&placing[at])? {
                Step::Placed => {
                    // The hold goes now that the value is counted.
                    drop(placing.swap_remove(at));
                    count(&counters.values_placed, 1);
                    *placed += 1;
                }
                Step::GivenUp => {
                    drop(placing.swap_remove(at));
                    count(&counters.values_given_up, 1);
                }
                Step::PutOff => at += 1,
                Step::Refused(error) => {
                    refused.get_or_insert(error);
                    at += 1;
                }
            }
        }
        Ok(())
    }

    /// The page step that places one value (`transactions.md` PV7, with
    /// its three checks).
    fn place(&self, ready: &Ready) -> Result<Step, FlushError> {
        let Stores {
            pool,
            values,
            versions,
            ..
        } = self.stores;
        let page = ready.page;
        let Ok(change) = page::placing(page, &ready.cell, ready.pointer) else {
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
        let covered = values.covers(ready.pointer);
        let step = pool.page_step(page);
        let Some(old) = step.current() else {
            return Ok(Step::GivenUp);
        };
        if !page::is_due(&old, &change).unwrap_or(false) {
            return Ok(Step::GivenUp);
        }
        if step.private_copies() > 0 || !covered {
            count(&self.state.counters.steps_put_off, 1);
            return Ok(Step::PutOff);
        }
        let mut blank = pool.blank();
        blank.copy_from(&old);
        if page::apply_step(&mut blank, self.size, &change).is_err() {
            return Ok(Step::GivenUp);
        }
        blank.detach(ready.cell.pending.id());

        if let Err(error) = self.append_change(&record) {
            drop(blank);
            drop(step);
            drop(counts);
            return self.record_not_written(error).map(Step::Refused);
        }
        step.replace(&old, blank)
            .map_err(|source| FlushError::StepRefused { page, source })?;
        drop(step);
        let mut acquired = ValueChange::new();
        acquired.acquire(ready.pointer);
        // The change is applied whatever this returns: an error says that
        // a count was found wrong, and the file is then kept.
        let _ = counts.apply(&acquired, versions.latest());
        Ok(Step::Placed)
    }

    /// What a page step does when the append of its record failed, with
    /// its locks given back. Nothing was installed.
    ///
    /// - The record is not in the log, and the log takes none until it has
    ///   been rolled: the value stays ready, and the step is made later.
    ///   If the append was taken back, a crash could still bring the
    ///   record back, so the log is settled first (`wal`, "When the file
    ///   layer fails"). What the log answered is returned as the refusal
    ///   of the step. Where the log had no file that takes items, that is
    ///   what left it so, if the log has kept it
    ///   ([`LogStore::refusal`](crate::wal::LogStore::refusal)).
    /// - The log cannot find out what became of the append: the record may
    ///   be in the log, so the value keeps its hold for as long as the
    ///   session lasts, and the log takes nothing more in it. That is the
    ///   caller's to know, and is returned as the error of the call.
    fn record_not_written(&self, error: LogError) -> Result<LogError, FlushError> {
        let log = self.stores.log;
        match error {
            LogError::TakenBack { .. } => {
                log.settle()?;
                Ok(error)
            }
            LogError::NotAppended { .. } => Ok(error),
            LogError::NoActiveFile => Ok(log.refusal().unwrap_or(error)),
            error => Err(error.into()),
        }
    }
}
