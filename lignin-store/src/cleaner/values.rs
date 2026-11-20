//! The cleaner's pass over the value log: which file is cleaned, the steps
//! of a cleaning in their order, and the two-step deletion of the files
//! that hold no live value any more.

use lignin_format::{manifest::FileState, value_log::FIRST_RECORD_OFFSET, FileId};

use super::{
    clean::Cleaner,
    config::count,
    error::CleanerError,
    space::{is_at_least, is_over, ValueSpace},
    state::{lock, Work},
};
use crate::{
    flush::Stores,
    values::{Cleaned, ValueOwners, ValueStoreError, ValueWriter},
};

/// How many answers of [`ValueStore::retire`](crate::values::ValueStore::retire)
/// the cleaner keeps while it waits for the operations that were running
/// when each was given. An answer that no older one has room for is not
/// asked for: the next pass asks again.
const MAX_TOKENS: usize = 8;

/// After how many passes that would have asked the value log the same
/// question as the pass before them it is asked again: a file that a
/// checkpoint's hold kept is free once the hold is given back, and nothing
/// else tells the cleaner of that.
const ASK_AGAIN_AFTER: u32 = 16;

/// What one pass over the value log did ([`Cleaner::pass_values`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValuePass {
    /// The file that was cleaned, with what the cleaning did, if the dead
    /// space of the value log was above its target and a file was worth
    /// it.
    pub cleaned: Option<(FileId, Cleaned)>,
    /// The files that were taken out of the store.
    pub dropped: Vec<FileId>,
}

impl ValuePass {
    /// Whether the pass moved anything on.
    pub fn progressed(&self) -> bool {
        self.cleaned.is_some_and(|(_, cleaned)| cleaned.moved != 0) || !self.dropped.is_empty()
    }
}

impl<'s> Cleaner<'s> {
    /// What the value log holds now: the bytes of its values, and those of
    /// them that a cell of the latest version points to.
    pub fn value_space(&self) -> Result<ValueSpace, CleanerError> {
        let values = self.stores().values;
        let counts = values.counts();
        let mut space = ValueSpace::default();
        for file in values.files_named()? {
            let held = file.valid_len().saturating_sub(FIRST_RECORD_OFFSET);
            space.files += 1;
            space.bytes += held;
            let counted = counts.binary_search_by_key(&file.id(), |count| count.file);
            if file.state() != FileState::Open && counted.is_err() {
                space.waiting_bytes += held;
            }
        }
        space.live_bytes = counts.iter().map(|count| count.bytes).sum();
        Ok(space)
    }

    /// One pass over the value log (`transactions.md` CL5):
    ///
    /// 1. The files that earlier passes found empty, and that no running
    ///    operation can still read from, are taken out of the store and
    ///    deleted. A file that is being filled is closed if too much of
    ///    it is dead.
    /// 2. If the dead space of the value log is above its target, the
    ///    closed file with the largest share of dead bytes is cleaned: its
    ///    live values are copied to the cleaner's own file, the copies are
    ///    made durable and a manifest covers them, and `owners` gives each
    ///    owning cell the pointer to its copy in a page step.
    /// 3. The log is made durable, so that the rewrites which emptied a
    ///    file are, and the value log is asked which files are empty for
    ///    every reader from now on. They are dropped once the operations
    ///    that are running now have ended: at once if none is, and by a
    ///    later pass otherwise.
    ///
    /// `owners` is every kind of owner the store has: the cells of tree
    /// pages and the entries of flat leaves each find nothing in a page of
    /// the other kind ([`BothOwners`](super::BothOwners)).
    ///
    /// **The caller holds no operation guard.** The pass takes one itself
    /// for step 2, in which the owners read pages
    /// ([`Published::guarded`](crate::flush::Published::guarded)), and has
    /// given it back before step 3: a guard that is held over the whole
    /// pass is one of before the file was emptied, and the file would
    /// wait for the pass after it.
    ///
    /// One pass of the cleaner runs at a time, over the segments or over
    /// the value log.
    pub fn pass_values<O: ValueOwners + ?Sized>(
        &self,
        owners: &O,
    ) -> Result<ValuePass, CleanerError> {
        let mut work = lock(&self.state.work);
        let counters = &self.state.counters;
        count(&counters.value_passes, 1);
        let mut pass = ValuePass::default();
        let ended = self.pass_values_with(&mut work, owners, &mut pass);
        if ended.is_err() {
            count(&counters.errors, 1);
        }
        ended.map(|()| pass)
    }

    fn pass_values_with<O: ValueOwners + ?Sized>(
        &self,
        work: &mut Work,
        owners: &O,
        pass: &mut ValuePass,
    ) -> Result<(), CleanerError> {
        let Stores {
            values, versions, ..
        } = self.stores();
        let counters = &self.state.counters;
        self.drop_values(work, pass)?;
        self.close_dead_files(work)?;

        let worst = self.worst_value_file()?;
        let mut cleaned = None;
        if let Some(file) = worst {
            versions.guarded(&mut || cleaned = Some((file, self.clean_file(work, file, owners))));
        }
        let mut failed = None;
        match cleaned {
            Some((file, Ok(cleaned))) => pass.cleaned = Some((file, cleaned)),
            // The values that were moved stand, and so does the count of
            // the file: the pass goes on to what can be dropped.
            Some((
                file,
                Err(CleanerError::Values(ValueStoreError::OwnersNotFound { cleaned, .. })),
            )) => {
                pass.cleaned = Some((file, cleaned));
                failed = Some(CleanerError::Values(ValueStoreError::OwnersNotFound {
                    file,
                    cleaned,
                }));
            }
            Some((_, Err(error))) => return Err(error),
            None => {}
        }
        if let Some((_, cleaned)) = pass.cleaned {
            count(&counters.value_files_cleaned, 1);
            count(&counters.values_moved, cleaned.moved);
            count(&counters.values_left, cleaned.gone + cleaned.busy);
            count(&counters.value_bytes_copied, cleaned.copied_bytes);
            self.wrote(cleaned.copied_bytes);
        }

        // A closed file that no count lists holds no live value: a
        // cleaning emptied it, or the commits did.
        let counted: Vec<FileId> = values.counts().iter().map(|count| count.file).collect();
        let own = work.values.as_ref().map(ValueWriter::file_id);
        let empty = values.files_named()?.into_iter().any(|file| {
            file.state() != FileState::Open
                && Some(file.id()) != own
                && counted.binary_search(&file.id()).is_err()
        });
        if empty && work.value_tokens.len() < MAX_TOKENS {
            // A file is deleted only once the change that emptied it is
            // durable: a crash would otherwise bring back pages that point
            // into a file that is gone.
            let mark = values.applied();
            if work.values_synced != Some(mark) {
                self.write_back.sync_log()?;
                work.values_synced = Some(mark);
            }
            // The three questions, in their order (PN6): the oldest pin,
            // then the epoch, and the oldest guard when a later pass looks.
            let asked = (values.retire(versions.oldest_pin()), mark);
            // The same answer drops what it dropped before, unless a hold
            // on the files was given back meanwhile: it is asked for again
            // now and then, and not in every pass.
            let again = work.values_asked == Some(asked);
            work.asks_spared = match again {
                true => work.asks_spared + 1,
                false => 0,
            };
            if !again || work.asks_spared >= ASK_AGAIN_AFTER {
                work.asks_spared = 0;
                work.values_asked = Some(asked);
                work.value_tokens
                    .push_back((asked.0, versions.next_epoch()));
                // With no operation running, nothing is waited for.
                self.drop_values(work, pass)?;
            }
        }
        failed.map_or(Ok(()), Err)
    }

    /// The closed file with the largest share of dead bytes, if the dead
    /// space that a cleaning can give back is above its target.
    ///
    /// That is the dead space of the files that can be cleaned and of
    /// those that are being filled. A file that holds no live value and
    /// waits to be deleted is left out, and so is one that no cleaning
    /// helps ([`ValueStore::cleanable`](crate::values::ValueStore::cleanable)):
    /// their bytes are dead whatever is moved out of the other files, and
    /// counted, they would have file after file cleaned for nothing while
    /// a pin keeps them.
    fn worst_value_file(&self) -> Result<Option<FileId>, CleanerError> {
        let values = self.stores().values;
        let cleanable = values.cleanable(0);
        let dead_of = |len: u64, live: u64| len.saturating_sub(live);
        let (mut dead, mut all) = (0u64, 0u64);
        for file in &cleanable {
            all += file.len;
            dead += dead_of(file.len, file.live_bytes);
        }
        let named = values.files_named()?;
        for file in named.iter().filter(|file| file.state() == FileState::Open) {
            let len = file.valid_len().saturating_sub(FIRST_RECORD_OFFSET);
            let live = values.count(file.id()).map_or(0, |count| count.bytes);
            all += len;
            dead += dead_of(len, live);
        }
        if !is_over(dead, all, self.state.config.value_dead_percent) {
            return Ok(None);
        }
        // The first is the one with the largest share. A file of which
        // less than one byte in a hundred is dead is not worth a cleaning.
        let worst = cleanable.first();
        Ok(worst
            .filter(|file| is_at_least(dead_of(file.len, file.live_bytes), file.len, 1))
            .map(|file| file.file))
    }

    /// Closes the value-log files that are being filled, by write-back or
    /// with the cleaner's copies, and of which too much is dead: a file is
    /// not cleaned while it is written.
    fn close_dead_files(&self, work: &mut Work) -> Result<(), CleanerError> {
        let values = self.stores().values;
        let config = &self.state.config;
        let named = values.files_named()?;
        let held = |len: u64| len.saturating_sub(FIRST_RECORD_OFFSET);
        let all: u64 = named.iter().map(|file| held(file.valid_len())).sum();
        let own = work.values.as_ref().map(ValueWriter::file_id);
        for file in named.iter().filter(|file| file.state() == FileState::Open) {
            let len = held(file.valid_len());
            let live = values.count(file.id()).map_or(0, |count| count.bytes);
            let dead = len.saturating_sub(live);
            let closes = is_at_least(dead, all, config.seal_dead_percent)
                || is_at_least(len, all, config.max_segment_percent);
            if len < config.min_segment_len || !closes {
                continue;
            }
            let closed = match Some(file.id()) == own {
                true => {
                    let writer = work.values.take().expect("the cleaner's file");
                    writer.finish()?;
                    true
                }
                false => self.write_back.close_value_file(file.id())?,
            };
            count(&self.state.counters.value_files_closed, u64::from(closed));
        }
        Ok(())
    }

    /// Takes out of the store the value-log files that the value log said
    /// were empty for every reader, now that the operations which were
    /// running then have ended, and has them deleted.
    fn drop_values(&self, work: &mut Work, pass: &mut ValuePass) -> Result<(), CleanerError> {
        let Stores {
            values, versions, ..
        } = self.stores();
        let oldest_guard = versions.oldest_guard();
        while let Some((retired, _)) = work
            .value_tokens
            .pop_front_if(|(_, epoch)| *epoch <= oldest_guard)
        {
            pass.dropped.extend(values.drop_empty(retired)?);
        }
        if !pass.dropped.is_empty() {
            count(
                &self.state.counters.value_files_dropped,
                pass.dropped.len() as u64,
            );
            match values.commit(&mut *self.write_back.manifest()?) {
                // The new manifest is in force, and a later replacement
                // deletes the file.
                Ok(()) | Err(ValueStoreError::NotDeleted { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    /// Cleans the closed file `file` into the cleaner's own.
    fn clean_file<O: ValueOwners + ?Sized>(
        &self,
        work: &mut Work,
        file: FileId,
        owners: &O,
    ) -> Result<Cleaned, CleanerError> {
        let values = self.stores().values;
        if work.values.as_ref().is_none_or(ValueWriter::is_full) {
            if let Some(full) = work.values.take() {
                full.finish()?;
            }
            let made = values.create()?;
            values.commit(&mut *self.write_back.manifest()?)?;
            work.values = Some(made);
        }
        let writer = work.values.as_mut().expect("a file was just made");
        let cleaned = (|| -> Result<Cleaned, CleanerError> {
            let copies = values.copy_live(file, writer, owners)?;
            if !copies.is_empty() {
                // Every copy is durable, under a manifest that covers it,
                // before a page points at one.
                writer.sync()?;
                values.commit(&mut *self.write_back.manifest()?)?;
            }
            Ok(values.install(copies, owners)?)
        })();
        let not_found = matches!(
            cleaned,
            Err(CleanerError::Values(ValueStoreError::OwnersNotFound { .. }))
        );
        if cleaned.is_err() && !not_found {
            // The copies are given up, and the next value goes to another
            // file: a sync of this one may have failed.
            work.values = None;
        }
        cleaned
    }
}
