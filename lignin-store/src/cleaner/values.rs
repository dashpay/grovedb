//! The cleaner's pass over the value log: which file is cleaned, the steps
//! of a cleaning in their order, and the two-step deletion of the files
//! that hold no live value any more.

use lignin_format::{manifest::FileState, value_log::FIRST_RECORD_OFFSET, FileId};

use super::{
    clean::Cleaner,
    config::count,
    error::CleanerError,
    space::{is_over, ValueSpace},
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
        let mut space = ValueSpace::default();
        for file in values.files_named()? {
            space.files += 1;
            space.bytes += file.valid_len().saturating_sub(FIRST_RECORD_OFFSET);
        }
        space.live_bytes = values.counts().iter().map(|count| count.bytes).sum();
        Ok(space)
    }

    /// One pass over the value log (`transactions.md` CL5):
    ///
    /// 1. The files that earlier passes found empty, and that no running
    ///    operation can still read from, are taken out of the store and
    ///    deleted.
    /// 2. If the dead space of the value log is above its target, the
    ///    closed file with the largest share of dead bytes is cleaned: its
    ///    live values are copied to the cleaner's own file, the copies are
    ///    made durable and a manifest covers them, and `owners` gives each
    ///    owning cell the pointer to its copy in a page step.
    /// 3. The log is made durable, so that the rewrites which emptied a
    ///    file are, and the value log is asked which files are empty for
    ///    every reader from now on. The next passes drop them once the
    ///    operations that are running now have ended.
    ///
    /// `owners` is every kind of owner the store has: the cells of tree
    /// pages and the entries of flat leaves each find nothing in a page of
    /// the other kind ([`BothOwners`](super::BothOwners)). The caller
    /// holds an operation guard for the pass if `owners` asks for one.
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
        let config = &self.state.config;
        let counters = &self.state.counters;
        self.drop_values(work, pass)?;

        let space = self.value_space()?;
        let worst = match is_over(space.dead_bytes(), space.bytes, config.value_dead_percent) {
            true => values.cleanable(1).first().map(|file| file.file),
            false => None,
        };
        let cleaned = worst.map(|file| (file, self.clean_file(work, file, owners)));
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
            self.write_back.sync_log()?;
            // The three questions, in their order (PN6): the oldest pin,
            // then the epoch, and the oldest guard when a later pass looks.
            let retired = values.retire(versions.oldest_pin());
            let epoch = versions.next_epoch();
            work.value_tokens.push_back((retired, epoch));
        }
        failed.map_or(Ok(()), Err)
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
