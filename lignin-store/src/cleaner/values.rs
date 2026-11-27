//! The cleaner's pass over the value log: which file is cleaned, the steps
//! of a cleaning in their order, and the two-step deletion of the files
//! that hold no live value any more.

use lignin_format::{
    manifest::{FileState, LiveFile},
    value_log::FIRST_RECORD_OFFSET,
    FileId,
};

use super::{
    clean::Cleaner,
    config::count,
    config::DeadClass,
    error::CleanerError,
    space::{is_at_least, is_over, ValueSpace},
    state::{lock, Work},
};
use crate::{
    flush::Stores,
    values::{Cleanable, Cleaned, ValueOwners, ValueStore, ValueStoreError, ValueWriter},
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

/// For how many passes a file is set aside after a cleaning that moved
/// nothing, because write transactions held copies of the pages of its
/// values: it is looked at again this many passes later. Each try appends
/// the file's live values again, and the copies are dead. The wait is the
/// same however many files are set aside.
pub(super) const BUSY_PASSES: u64 = 8;

/// The share of a file's bytes, in a hundred, that are dead from which a
/// file is cleaned in the same pass as the file most worth it, while the
/// copies of the pass's other files stay within
/// [`CleanerConfig::max_pass_bytes`](super::CleanerConfig::max_pass_bytes).
/// Such a file costs little to clean, and files that a burst of commits
/// empties come faster than one a pass.
pub(super) const SWEEP_DEAD_PERCENT: u8 = 90;

/// The bytes of the values of a value-log file: what follows its header.
fn value_bytes(file: &LiveFile) -> u64 {
    file.valid_len().saturating_sub(FIRST_RECORD_OFFSET)
}

/// The bytes of the live values of the file `file`.
fn live_bytes(values: &ValueStore, file: FileId) -> u64 {
    values.count(file).map_or(0, |count| count.bytes)
}

/// What one pass over the value log did ([`Cleaner::pass_values`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValuePass {
    /// The file that was cleaned first, with what the cleaning did, if the
    /// dead space of the value log was above its target and a file was
    /// worth it.
    pub cleaned: Option<(FileId, Cleaned)>,
    /// The files that were cleaned after it in the same pass, each at
    /// least [`SWEEP_DEAD_PERCENT`] dead, in the order they were cleaned.
    pub swept: Vec<(FileId, Cleaned)>,
    /// The files that were taken out of the store.
    pub dropped: Vec<FileId>,
}

impl ValuePass {
    /// Whether the pass moved anything on.
    pub fn progressed(&self) -> bool {
        let moved = |(_, cleaned): &(FileId, Cleaned)| cleaned.moved != 0;
        self.cleaned.as_ref().is_some_and(moved)
            || self.swept.iter().any(moved)
            || !self.dropped.is_empty()
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
            let held = value_bytes(&file);
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
    ///    closed file with the largest share of dead bytes is cleaned, if
    ///    it is worth it ([`Self::value_files_to_clean`]), and after it the
    ///    files that are nearly empty: their live values are copied to the
    ///    cleaner's own file, the copies are
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
        let versions = self.stores().versions;
        let counters = &self.state.counters;
        work.value_passes += 1;
        self.drop_values(work, pass)?;
        self.close_dead_files(work)?;

        let mut failed = None;
        for taken in self.value_files_to_clean(work)? {
            let file = taken.file;
            let mut cleaned = None;
            versions.guarded(&mut || cleaned = Some(self.clean_file(work, file, owners)));
            let cleaned = match cleaned {
                Some(Ok(cleaned)) => cleaned,
                // The values that were moved stand, and so does the count
                // of the file: the pass goes on to what can be dropped.
                Some(Err(
                    error @ CleanerError::Values(ValueStoreError::OwnersNotFound {
                        cleaned, ..
                    }),
                )) => {
                    failed = Some(error);
                    cleaned
                }
                Some(Err(error)) => return Err(error),
                None => continue,
            };
            count(&counters.value_files_cleaned, 1);
            count(&counters.values_moved, cleaned.moved);
            count(&counters.values_left, cleaned.gone + cleaned.busy);
            count(&counters.value_bytes_copied, cleaned.copied_bytes);
            let class = DeadClass::of(taken.len.saturating_sub(taken.live_bytes), taken.len);
            count(&counters.value_files_by_dead[class.index()], 1);
            count(
                &counters.value_bytes_by_dead[class.index()],
                cleaned.copied_bytes,
            );
            self.wrote(cleaned.copied_bytes);
            work.busy_files.remove(&file);
            if cleaned.moved == 0 && cleaned.busy != 0 {
                work.busy_files
                    .insert(file, work.value_passes + BUSY_PASSES);
            }
            match pass.cleaned {
                None => pass.cleaned = Some((file, cleaned)),
                Some(_) => pass.swept.push((file, cleaned)),
            }
            if failed.is_some() {
                break;
            }
        }

        // What the cleaning found is what the pass ends with, whatever
        // fails afterwards: in this session the file is not looked at
        // again while its count stands.
        let dropped = self.drop_emptied(work, pass);
        failed.map_or(dropped, Err)
    }

    /// Step 3 of [`Self::pass_values`].
    fn drop_emptied(&self, work: &mut Work, pass: &mut ValuePass) -> Result<(), CleanerError> {
        let Stores {
            values, versions, ..
        } = self.stores();
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
            if work.values_asked == Some(asked) && work.asks_spared + 1 < ASK_AGAIN_AFTER {
                work.asks_spared += 1;
            } else {
                work.asks_spared = 0;
                work.values_asked = Some(asked);
                work.value_tokens
                    .push_back((asked.0, versions.next_epoch()));
                // With no operation running, nothing is waited for.
                self.drop_values(work, pass)?;
            }
        }
        Ok(())
    }

    /// The closed files a pass cleans, in the order it cleans them. None
    /// unless the dead space that a cleaning can give back is above its
    /// target ([`CleanerConfig::value_dead_percent`]).
    ///
    /// That is the dead space of the files that can be cleaned and of
    /// those that are being filled. A file that holds no live value and
    /// waits to be deleted is left out, and so is one that no cleaning
    /// helps ([`ValueStore::cleanable`](crate::values::ValueStore::cleanable)):
    /// their bytes are dead whatever is moved out of the other files, and
    /// counted, they would have file after file cleaned for nothing while
    /// a pin keeps them.
    ///
    /// The first is the file with the largest share of dead bytes, if that
    /// share is at least the floor
    /// ([`CleanerConfig::value_min_file_dead_percent`]): a file that is
    /// mostly live costs many copies for little space. Over the ceiling
    /// ([`CleanerConfig::value_ceiling_dead_percent`]) a file one byte in
    /// a hundred of which is dead is enough, so that a log whose deaths
    /// are spread over every file is held there. With a target of nothing
    /// dead, any dead byte is enough. After the first, the files that are
    /// at least [`SWEEP_DEAD_PERCENT`] dead follow, most dead first, as
    /// long as their live bytes stay within
    /// [`CleanerConfig::max_pass_bytes`].
    ///
    /// A file whose last cleaning moved nothing, because write
    /// transactions held copies of the pages of its values, is passed
    /// over for [`BUSY_PASSES`] passes.
    ///
    /// [`CleanerConfig::value_dead_percent`]: super::CleanerConfig::value_dead_percent
    /// [`CleanerConfig::value_min_file_dead_percent`]: super::CleanerConfig::value_min_file_dead_percent
    /// [`CleanerConfig::value_ceiling_dead_percent`]: super::CleanerConfig::value_ceiling_dead_percent
    /// [`CleanerConfig::max_pass_bytes`]: super::CleanerConfig::max_pass_bytes
    fn value_files_to_clean(&self, work: &mut Work) -> Result<Vec<Cleanable>, CleanerError> {
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
            let len = value_bytes(file);
            all += len;
            dead += dead_of(len, live_bytes(values, file.id()));
        }
        let now = work.value_passes;
        work.busy_files
            .retain(|file, _| cleanable.iter().any(|held| held.file == *file));
        let config = &self.state.config;
        if !is_over(dead, all, config.value_dead_percent) {
            return Ok(Vec::new());
        }
        let floor = match config.value_dead_percent {
            0 => 0,
            _ if is_over(dead, all, config.value_ceiling_dead_percent) => 1,
            _ => config.value_min_file_dead_percent.max(1),
        };
        let dead_in = |file: &Cleanable| dead_of(file.len, file.live_bytes);
        // They come with the largest share first: those the floor lets a
        // pass take, and of them those that are not set aside.
        let worth: Vec<&Cleanable> = cleanable
            .iter()
            .filter(|file| is_at_least(dead_in(file), file.len, floor))
            .collect();
        let free: Vec<&Cleanable> = worth
            .iter()
            .copied()
            .filter(|held| {
                work.busy_files
                    .get(&held.file)
                    .is_none_or(|from| *from <= now)
            })
            .collect();
        if free.is_empty() && !worth.is_empty() {
            count(&self.state.counters.value_passes_all_busy, 1);
        }
        let Some(first) = free.first() else {
            return Ok(Vec::new());
        };
        let mut taken = vec![**first];
        let mut copies = 0u64;
        for file in &free[1..] {
            let nearly_empty = is_at_least(dead_in(file), file.len, SWEEP_DEAD_PERCENT);
            if !nearly_empty || copies + file.live_bytes > config.max_pass_bytes {
                break;
            }
            copies += file.live_bytes;
            taken.push(**file);
        }
        Ok(taken)
    }

    /// Closes the value-log files that are being filled, by write-back or
    /// with the cleaner's copies, and of which too much is dead: a file is
    /// not cleaned while it is written.
    fn close_dead_files(&self, work: &mut Work) -> Result<(), CleanerError> {
        let values = self.stores().values;
        let config = &self.state.config;
        let named = values.files_named()?;
        let all: u64 = named.iter().map(value_bytes).sum();
        let own = work.values.as_ref().map(ValueWriter::file_id);
        for file in named.iter().filter(|file| file.state() == FileState::Open) {
            let len = value_bytes(file);
            let dead = len.saturating_sub(live_bytes(values, file.id()));
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
        let before = pass.dropped.len();
        while let Some((retired, _)) = work
            .value_tokens
            .pop_front_if(|(_, epoch)| *epoch <= oldest_guard)
        {
            pass.dropped.extend(values.drop_empty(retired)?);
        }
        // A pass looks twice: only what this look dropped is counted, and
        // named no more.
        let dropped = (pass.dropped.len() - before) as u64;
        if dropped != 0 {
            count(&self.state.counters.value_files_dropped, dropped);
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
        let cleaned = self.copy_and_install(writer, file, owners);
        let not_found = |error: &CleanerError| {
            matches!(
                error,
                CleanerError::Values(ValueStoreError::OwnersNotFound { .. })
            )
        };
        if cleaned.as_ref().is_err_and(|error| !not_found(error)) {
            // The copies are given up, and the next value goes to another
            // file: a sync of this one may have failed. The file is
            // synced by its writer, through the handle that appended, and
            // closed, so that a later pass can clean it
            // (`ValueWriter::give_up`).
            if let Some(writer) = work.values.take() {
                count(&self.state.counters.value_files_given_up, 1);
                let _ = writer.give_up();
            }
        }
        cleaned
    }

    /// The steps of a cleaning in their order: the live values of `file`
    /// are copied to `writer`, the copies are made durable under a
    /// manifest that covers them, and each owner is given its pointer.
    fn copy_and_install<O: ValueOwners + ?Sized>(
        &self,
        writer: &mut ValueWriter,
        file: FileId,
        owners: &O,
    ) -> Result<Cleaned, CleanerError> {
        let values = self.stores().values;
        let copies = values.copy_live(file, writer, owners)?;
        if !copies.is_empty() {
            // Every copy is durable, under a manifest that covers it,
            // before a page points at one.
            writer.sync()?;
            values.commit(&mut *self.write_back.manifest()?)?;
        }
        Ok(values.install(copies, owners)?)
    }
}
