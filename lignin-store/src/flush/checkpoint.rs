//! Table checkpoints, and the log's files: what a checkpoint may cover, the
//! order in which the sample, the pages, the checkpoint file, the log and
//! the manifest are dealt with, and which log files can go afterwards
//! (`page-table.md` K1 to K15, `log-format.md` LG29).

use std::sync::atomic::Ordering;

use lignin_format::{CommitVersion, PageId};

use super::{
    config::count,
    error::FlushError,
    page::PageKind,
    state::{lock, segments_of, Checkpoints, Named},
    write_back::{Stores, WriteBack},
};
use crate::{
    segment::SegmentStoreError,
    table::{
        CheckpointInfo, CountSample, Link, LinkError, LinkKind, LogDurable, LogPlace,
        PageTableError, ValueCount,
    },
    values::CountStep,
    wal::LogError,
};

/// How often [`WriteBack::roll_log`] goes round before it gives up: as
/// often as [`LogStore::roll`](crate::wal::LogStore::roll) does.
const ROLL_ROUNDS: usize = 6;

/// How often a table checkpoint rolls a log that has no file that takes
/// items before it appends its mark.
const MARK_ROLLS: usize = 3;

/// What a call of [`WriteBack::checkpoint`] came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checkpointed {
    /// The manifest names a new table checkpoint.
    Installed {
        /// The commit version it covers.
        covers: CommitVersion,
        /// The place of its sample of the value counts.
        place: LogPlace,
        /// How many log files were taken out of the log afterwards.
        log_files_dropped: usize,
    },
    /// A page version that had to be in the checkpoint could not be written
    /// back yet: it has a pending value on a page that a write transaction
    /// holds a private copy of. Nothing was written, the checkpoint that
    /// was in force still is, and the call can be made again later.
    PutOff {
        /// How many page versions stood in the way.
        blocked: usize,
    },
}

impl<'s> WriteBack<'s> {
    /// Tells write-back which table checkpoint the manifest names, when the
    /// store has been opened and before anything is appended to the log:
    /// what [`PageTable::load`](crate::table::PageTable::load) returned,
    /// or `None` if the manifest names none.
    ///
    /// Write-back keeps the log from the place of that checkpoint's sample
    /// on, and the whole log while there is none, and installs no
    /// checkpoint that covers less or whose sample is from an earlier
    /// place.
    pub fn adopt(&self, in_force: Option<&CheckpointInfo>) -> Result<(), FlushError> {
        Ok(self.state.adopt(self.stores.log, in_force)?)
    }

    /// Makes the log durable, and tells the value log that every change to
    /// its counts whose record was in the log is durable
    /// ([`ValueStore::mark_durable`](crate::values::ValueStore::mark_durable)).
    ///
    /// Whoever needs the log durable in a session with write-back calls
    /// this and not [`LogStore::sync`](crate::wal::LogStore::sync) itself:
    /// `flush()`, and a commit whose sync policy asked for a sync, once it
    /// has synced.
    pub fn sync_log(&self) -> Result<LogDurable, FlushError> {
        let Stores { values, log, .. } = self.stores;
        // Every change that is counted has its record in the log already:
        // a change point appends its record before it counts.
        let mark = values.applied();
        let durable = log.sync()?;
        values.mark_durable(mark)?;
        Ok(durable)
    }

    /// Syncs the log if its policy has a sync due
    /// ([`LogStore::sync_if_due`](crate::wal::LogStore::sync_if_due)), as
    /// [`Self::sync_log`] does.
    pub fn sync_log_if_due(&self) -> Result<Option<LogDurable>, FlushError> {
        let Stores { values, log, .. } = self.stores;
        let mark = values.applied();
        let durable = log.sync_if_due()?;
        if durable.is_some() {
            values.mark_durable(mark)?;
        }
        Ok(durable)
    }

    /// Keeps the log's files going, when the file the log ends in is full,
    /// when an append to it failed, or when the session has no file yet:
    /// the log is made durable and turned to its next file, and a file to
    /// turn to after that is created and named by a durable manifest.
    /// Returns whether the log turned to another file.
    ///
    /// It is [`LogStore::roll`] made of its parts, because a turn moves the
    /// end of the log. The turn is made under the lock on the value counts,
    /// which a commit holds from the moment it asks for the place of its
    /// group until the group is in, so no commit finds its place taken by
    /// a turn. The syncs, the new file and the manifest are dealt with
    /// outside that lock.
    ///
    /// After a failed append this is what makes the cut of CM9: the log
    /// takes no item until it has returned `Ok`.
    ///
    /// [`LogStore::roll`]: crate::wal::LogStore::roll
    pub fn roll_log(&self) -> Result<bool, FlushError> {
        let log = self.stores.log;
        if !log.is_due() {
            return Ok(false);
        }
        let mut manifest = self.manifest()?;
        let mut turned = false;
        let mut in_the_way = None;
        for _ in 0..ROLL_ROUNDS {
            self.sync_log()?;
            in_the_way = match self.turn_log() {
                Ok(now) => {
                    turned |= now;
                    None
                }
                // The log has no file to turn to that a durable manifest
                // names, or the file it ends in is not named closed yet:
                // the rest of this round sees to both.
                Err(FlushError::Log(
                    error @ (LogError::NoSpare
                    | LogError::SpareNotNamed { .. }
                    | LogError::CutNotDurable { .. }),
                )) => Some(error),
                Err(error) => return Err(error),
            };
            match log.create() {
                Ok(_) | Err(LogError::SpareExists { .. }) => {}
                Err(error) => return Err(error.into()),
            }
            log.commit(&mut manifest)?;
            if in_the_way.is_none() && !log.is_due() {
                return Ok(turned);
            }
        }
        // A turn is due again after every round. If the last round made
        // its turn, commits fill the files as fast as they are made:
        // nothing is wrong, and the next call goes on. Otherwise what
        // stood in the way of the last turn is the error.
        match in_the_way {
            None => Ok(turned),
            Some(error) => Err(error.into()),
        }
    }

    /// Turns the log to its next file if a turn is due, with every commit
    /// and every page step kept out.
    fn turn_log(&self) -> Result<bool, FlushError> {
        let Stores { values, log, .. } = self.stores;
        let _counts = values.step()?;
        Ok(log.rotate()?)
    }

    /// Turns the log where a table checkpoint begins, if the file it ends
    /// in holds a commit ([`LogStore::rotate_early`]). Returns whether it
    /// turned.
    ///
    /// Where the log has no file to turn to, one is made ready first: it
    /// is created, and a manifest names it. A recovery leaves the log
    /// without one ([`LogStore::begin`]), and so does a turn that used the
    /// file up. The turn itself is made with every commit and every page
    /// step kept out.
    ///
    /// **The checkpoint does not rest on this turn**, so what fails here
    /// does not fail it. Where the file cannot be created or named, or its
    /// start record cannot be appended, the log stays in the file it is
    /// in and the checkpoint is taken there, as it was before the log was
    /// turned for checkpoints. One refusal turns the log all the same: a
    /// start record that the device refused and that is in the file. The
    /// log is then in a file that takes no item, which the checkpoint
    /// looks for before its mark ([`Self::step_for_the_mark`]). So it is while the manifest is in doubt,
    /// which this turn does not settle. What stood in the way meets
    /// whoever needs it next: the turn away from a file that is full
    /// ([`Self::roll_log`]), or the manifest that names this checkpoint.
    /// Only a log that cannot say whether an item is in it ends the
    /// attempt ([`LogError::Unsettled`]), as it ends everything else in
    /// the session.
    ///
    /// [`LogStore::rotate_early`]: crate::wal::LogStore::rotate_early
    /// [`LogStore::begin`]: crate::wal::LogStore::begin
    fn turn_log_early(&self) -> Result<bool, FlushError> {
        let Stores { values, log, .. } = self.stores;
        if !log.holds_a_commit() {
            return Ok(false);
        }
        if !log.has_file_ready() {
            let mut manifest = lock(self.stores.manifest);
            // A manifest in doubt is settled by whoever needs the
            // manifest, and not for this turn.
            let created = manifest.manifest().is_ok()
                && matches!(log.create(), Ok(_) | Err(LogError::SpareExists { .. }));
            if created {
                // Whatever this answers, the question is whether a file
                // is ready now: a manifest that could not delete a file
                // it no longer names is in force all the same.
                let _ = log.commit(&mut manifest);
            }
            drop(manifest);
            if !log.has_file_ready() {
                return Ok(false);
            }
        }
        let _counts = values.step()?;
        match log.rotate_early() {
            Err(LogError::NotAppended { .. }) => Ok(false),
            turned => Ok(turned?),
        }
    }

    /// Takes the lock on the counts of live values, under which a table
    /// checkpoint appends its mark, with a log that has a file that takes
    /// items if a roll of the log gives it one.
    ///
    /// The log can have none here. The early turn of the checkpoint was
    /// refused its start record by a device that kept the record
    /// ([`Self::turn_log_early`]), or an append failed since the
    /// checkpoint began: one of its own pass, or one of a commit or a
    /// page step beside it. Nothing is appended to the log under the lock
    /// on the counts but by its holder, and the log is not turned under
    /// it by another: between a look here that finds a file that takes
    /// items and the append of the mark nobody else's append or turn
    /// comes. Whether the mark is appended is then the file layer's to
    /// say. A log that is found with no such file is
    /// rolled, with the lock given back, since a roll takes it, as a log
    /// is rolled after any append that failed ([`Self::roll_log`]). What
    /// stands in the way of a roll is the error of the checkpoint.
    ///
    /// The log is rolled [`MARK_ROLLS`] times at most. After each roll an
    /// append can fail again before the lock is held again, and a roll
    /// can end with a file that takes no item where a start record was
    /// refused and kept each time. A log that has no file after the last
    /// roll is left so, and the checkpoint fails with what the file layer
    /// answered the append that left it so ([`LogError::NoActiveFileSince`]).
    /// Whoever keeps the log rolls it at its next turn.
    ///
    /// While the log has a file that takes items this makes no operation
    /// on a file.
    fn step_for_the_mark(&self) -> Result<CountStep<'s>, FlushError> {
        let Stores { values, log, .. } = self.stores;
        let mut rolls = 0;
        loop {
            let step = values.step()?;
            let takes_items = !matches!(log.next_group(), Err(LogError::NoActiveFile));
            if takes_items {
                return Ok(step);
            }
            drop(step);
            if rolls == MARK_ROLLS {
                let error = log.refusal().unwrap_or(LogError::NoActiveFile);
                self.append_failed(&error);
                return Err(error.into());
            }
            self.roll_log()?;
            rolls += 1;
        }
    }

    /// Whether a table checkpoint is due: as many bytes were logged since
    /// the last one as the configuration says, or more log is held than
    /// it allows ([`Self::log_held`]).
    pub fn checkpoint_due(&self) -> bool {
        let config = &self.state.config;
        self.state.logged.load(Ordering::Relaxed) >= config.checkpoint_log_bytes
            || self.log_held() > config.max_log_held
    }

    /// The oldest version of which some change to a page is in no block,
    /// if there is one: what the pool lists as owed, and what was kept in
    /// the pool for readers and waits for the block of the next version of
    /// its page. A page that was freed since is not counted (T18).
    pub(super) fn floor(&self) -> Option<CommitVersion> {
        let Stores { pool, table, .. } = self.stores;
        // The pool is asked first: a version leaves its list only after
        // it was noted as kept for readers.
        let mut floor = pool.oldest_unwritten();
        let folded = lock(&self.state.kept).folded_pages();
        for (page, oldest, newest) in folded {
            // A page with a version that waits for the block of the next
            // has that next version in the pool, as its current one, until
            // the block is written. A page that has none was freed, and so
            // was one whose free link is newer than what waits. The pool
            // is asked in a page step, as background work asks it: a read
            // at the latest version is for an operation under its guard.
            let freed = match table.newest(page) {
                Some(link) if link.kind == LinkKind::Free && link.version.get() > newest => true,
                _ => pool.page_step(page).current().is_none(),
            };
            if freed {
                lock(&self.state.kept).unfold(page, newest);
                continue;
            }
            floor = match (floor, CommitVersion::new(oldest)) {
                (Some(owed), Some(waits)) => Some(owed.min(waits)),
                (owed, waits) => owed.or(waits),
            };
        }
        floor
    }

    /// How many bytes of log write-back holds: the log from the group of
    /// the oldest commit that no table checkpoint covers. It is what a
    /// recovery would read, and what no log file can be dropped from. 0
    /// when a checkpoint covers every commit that write-back was told of
    /// ([`Self::committed`]).
    pub fn log_held(&self) -> u64 {
        let before = lock(&self.state.kept).logged_before_uncovered();
        let logged = self.state.logged_in_all.load(Ordering::Relaxed);
        before.map_or(0, |before| logged.saturating_sub(before))
    }

    /// Whether every change of `page` up to the version `born`, which was
    /// owed when a sample was taken, is in a block now, or never will be
    /// because the page was freed.
    pub(super) fn is_in_block(&self, page: PageId, born: CommitVersion) -> bool {
        let Stores { pool, table, .. } = self.stores;
        // The pool is asked first, as for the floor.
        let mut owed = Vec::new();
        pool.unwritten_pages(born, &mut owed);
        let folded = lock(&self.state.kept).folded(page);
        let waits = owed.contains(&page) || folded.first().is_some_and(|&kept| kept <= born.get());
        match table.newest(page) {
            Some(newest) if newest.kind == LinkKind::Free && newest.version > born => true,
            Some(newest) => newest.version >= born && !waits,
            // No block of the page was ever linked. Its version leaves
            // what waits only when a commit frees the page (T18).
            None => !waits,
        }
    }

    /// Takes a table checkpoint and has the manifest name it, and then
    /// takes out of the log the files that nothing needs any more.
    ///
    /// The steps, in the order `page-table.md` 4.6 asks for:
    ///
    /// 1. The log is rolled if that is due. Then it is turned to its next
    ///    file, if the file it ends in holds a commit, once for each
    ///    checkpoint that is installed ([`LogStore::rotate_early`]); a
    ///    file to turn to is made ready first where the log has none.
    ///    Then a pass writes back what it can.
    /// 2. Under the lock on the value counts: the version *C* the
    ///    checkpoint will cover is fixed, the counts are sampled, the mark
    ///    is appended to the log, and the log is held from the end of the
    ///    mark, which is the sample's place. *C* is the newest version such
    ///    that no change of a commit up to it is still to be written back,
    ///    and never less than what the checkpoint in force covers.
    /// 3. A second pass writes back what was owed at that moment: every
    ///    page that a page step changed before the sample is among it. If
    ///    one of those page versions cannot be written yet, the attempt
    ///    ends here ([`Checkpointed::PutOff`]).
    /// 4. The checkpoint file is written, with the end of the log read
    ///    before the first chain and after the last.
    /// 5. The log is synced, after the copy.
    /// 6. The checkpoint is installed, with what that sync made durable.
    /// 7. The log files before the group of *C* + 1 and before the
    ///    sample's place are taken out of the log.
    ///
    /// One checkpoint is taken at a time, and a second call waits for the
    /// first.
    ///
    /// [`LogStore::rotate_early`]: crate::wal::LogStore::rotate_early
    pub fn checkpoint(&self) -> Result<Checkpointed, FlushError> {
        let checkpointed = self.try_checkpoint();
        if checkpointed.is_err() {
            count(&self.state.counters.checkpoints_failed, 1);
        }
        checkpointed
    }

    fn try_checkpoint(&self) -> Result<Checkpointed, FlushError> {
        let Stores {
            log,
            checkpoints,
            table,
            segments,
            versions,
            ..
        } = self.stores;
        let counters = &self.state.counters;
        let mut state = lock(&self.state.checkpoints);
        self.settle(&mut state)?;
        if self.state.segments_failed.load(Ordering::Acquire) {
            return Err(FlushError::SegmentsNotDurable);
        }
        self.roll_log()?;
        // The log goes on in its next file from here, once for each
        // checkpoint that is installed: every commit up to this moment is
        // in a file before that one, and is there for the pass to write
        // back.
        if !state.turned {
            state.turned = self.turn_log_early()?;
        }
        self.pass()?;

        let in_force = state.in_force.as_ref().map(|named| named.covers);
        let step = self.step_for_the_mark()?;
        let latest = versions.latest();
        let written = match self.floor() {
            Some(floor) => CommitVersion::new(floor.get().saturating_sub(1)).unwrap_or(latest),
            None => latest,
        };
        let covers = written
            .min(latest)
            .max(in_force.unwrap_or(CommitVersion::EMPTY));
        let owed: Vec<(PageId, CommitVersion)> = self
            .owed()
            .into_iter()
            .flat_map(|(page, versions)| versions.into_iter().map(move |born| (page, born)))
            .collect();
        let counts = step.counts();
        let mark = match log.append_mark(covers) {
            Ok(mark) => mark,
            Err(error) => {
                drop(step);
                self.append_failed(&error);
                return Err(error.into());
            }
        };
        let place = mark.end;
        if let Some(before) = state.last_sample
            && place < before
        {
            return Err(FlushError::SampleWentBack {
                before,
                taken: place,
            });
        }
        let hold = log.hold_from(Some(place))?;
        drop(step);
        state.last_sample = Some(place);

        self.pass()?;
        let blocked = owed
            .iter()
            .filter(|&&(page, born)| !self.is_in_block(page, born))
            .count();
        if blocked > 0 {
            count(&counters.checkpoints_put_off, 1);
            return Ok(Checkpointed::PutOff { blocked });
        }

        let sample = CountSample {
            place: Some(place),
            counts: counts
                .iter()
                .map(|count| ValueCount {
                    file: count.file,
                    values: count.values,
                    bytes: count.bytes,
                    miscounted: count.miscounted,
                })
                .collect(),
        };
        let info = checkpoints.write(table, covers, &sample, || log.end())?;
        count(
            &self.state.counters.checkpoint_bytes,
            info.named().valid_len(),
        );
        let named = Named {
            covers,
            place: Some(place),
            segments: segments_of(&info),
            _hold: hold,
        };
        let durable = match self.sync_log() {
            Ok(durable) => durable,
            Err(error) => {
                // The sync's error is the one the caller hears, whatever
                // becomes of the checkpoint that was written. Its deletion
                // is tried under the manifest as it is once a doubt was
                // settled, whatever a store answered the settling. If the
                // file cannot be removed then, the removal is tried again
                // when the table checkpoints next hear of a manifest, in
                // this session or as the store is opened. While the manifest
                // is still in doubt, because it could not be opened again,
                // the deletion is refused: the file stays, named by no
                // manifest, until the store is opened again. No log is
                // held for it, and the session installs no checkpoint
                // after a log that could not be made durable.
                drop(self.manifest());
                let _ = checkpoints.discard(info, &lock(self.stores.manifest));
                return Err(error);
            }
        };
        let mut manifest = self.manifest()?;
        match checkpoints.install(&info, durable, &mut manifest, segments) {
            Ok(())
            | Err(
                PageTableError::NotDeleted { .. }
                | PageTableError::Store(SegmentStoreError::NotDeleted { .. }),
            ) => {}
            Err(error) => {
                let in_doubt = manifest.manifest().is_err();
                let sync_failed = !in_doubt && segments.sync().is_err();
                if in_doubt {
                    // The directory has the old manifest or the new one.
                    // Both checkpoints keep their log until it is known.
                    state.in_doubt = Some((info, named));
                } else {
                    let _ = checkpoints.discard(info, &manifest);
                }
                drop(manifest);
                if sync_failed {
                    self.fail_segment();
                }
                return Err(error.into());
            }
        }
        drop(manifest);
        state.in_force = Some(named);
        state.turned = false;
        count(&counters.checkpoints, 1);
        self.state.logged.store(0, Ordering::Relaxed);
        let log_files_dropped = self.drop_log(covers, place)?;
        Ok(Checkpointed::Installed {
            covers,
            place,
            log_files_dropped,
        })
    }

    /// Finds out which checkpoint the manifest names after an installation
    /// that left it in doubt, and keeps the log for that one only.
    fn settle(&self, state: &mut Checkpoints) -> Result<(), FlushError> {
        let Some((info, named)) = state.in_doubt.take() else {
            return Ok(());
        };
        let manifest = match self.manifest() {
            Ok(manifest) => manifest,
            Err(error) => {
                state.in_doubt = Some((info, named));
                return Err(error);
            }
        };
        let is_named = manifest
            .manifest()?
            .table_checkpoint()
            .is_some_and(|table| table.file() == info.file());
        if is_named {
            state.in_force = Some(named);
            state.turned = false;
            count(&self.state.counters.checkpoints, 1);
        } else {
            // The checkpoint the log was turned for is gone: the next one
            // turns it again.
            state.turned = false;
            let _ = self.stores.checkpoints.discard(info, &manifest);
        }
        Ok(())
    }

    /// A sync of the segment that write-back is filling failed. Its writer
    /// is kept, so that every block of it stays readable, and no
    /// checkpoint is installed in this session any more.
    fn fail_segment(&self) {
        let mut files = lock(&self.state.files);
        if let Some(writer) = files.segment.take() {
            files.failed.push(writer);
        }
        count(&self.state.counters.segment_syncs_failed, 1);
        self.state.segments_failed.store(true, Ordering::Release);
    }

    /// Takes out of the log every file before the earliest place that
    /// something still needs, now that a checkpoint which covers `covers`
    /// and whose sample is at `place` is in force (`transactions.md` FL6):
    /// the group of the commit after `covers`, and the sample's place. A
    /// pending value needs no place of its own: its commit is not covered
    /// while it is pending. Returns how many files went.
    fn drop_log(&self, covers: CommitVersion, place: LogPlace) -> Result<usize, FlushError> {
        let log = self.stores.log;
        let next = covers.next().map(CommitVersion::get);
        let keep_from = {
            let mut kept = lock(&self.state.kept);
            kept.covered(covers.get());
            match next.and_then(|next| kept.group_place(next)) {
                Some(group) => group.min(place),
                // No commit after `covers` is in the log, as far as
                // write-back was told. If one is that it was not told of,
                // nothing is taken out.
                None if log.latest() == covers => place,
                None => return Ok(0),
            }
        };
        let dropped = match log.drop_before(keep_from) {
            Ok(dropped) => dropped,
            Err(LogError::NotDurable { .. } | LogError::UnknownFile { .. }) => Vec::new(),
            Err(error) => return Err(error.into()),
        };
        if !dropped.is_empty() {
            log.commit(&mut *self.manifest()?)?;
            count(&self.state.counters.log_files_dropped, dropped.len() as u64);
        }
        Ok(dropped.len())
    }

    /// Consolidates the chain of `page`: writes the base image that stands
    /// for the chain up to its newest link, and adds it to the chain right
    /// after that link (`page-table.md` T9). Returns whether an image was
    /// added: a chain that ends in a base image, a free link or nothing
    /// needs none.
    ///
    /// A pass keeps every chain it adds to within the thresholds by itself.
    /// This is for a chain that an earlier session wrote under other
    /// thresholds ([`Self::consolidate_long_chains`]), and for whoever
    /// wants a page read from one block. The image is built from the
    /// blocks of the chain, never from a frame, so it holds exactly what
    /// the chain up to that link gives.
    pub fn consolidate(&self, page: PageId) -> Result<bool, FlushError> {
        let table = self.stores.table;
        let mut files = lock(&self.state.files);
        let files = &mut *files;
        let Some(newest) = table.newest(page) else {
            return Ok(false);
        };
        let LinkKind::Delta(through) = newest.kind else {
            return Ok(false);
        };
        if self.read_chain(files, page, &newest)? != Some(PageKind::Tree) {
            return Ok(false);
        }
        files.image.clear();
        let pending = super::page::write_image(&files.scratch, &mut files.image)
            .map_err(FlushError::page(page))?;
        if pending {
            return Err(FlushError::PendingValue {
                page,
                born: newest.version,
            });
        }
        let location = self.append_image(files, page)?;
        match table.consolidate(page, through, location) {
            Ok(version) => {
                self.linked_image(files, page, version, location, true);
                Ok(true)
            }
            Err(LinkError::NotInChain { .. }) => {
                count(&self.state.counters.links_refused, 1);
                Ok(false)
            }
            Err(source) => Err(FlushError::Link {
                page,
                location,
                source,
            }),
        }
    }

    /// Consolidates every chain that has more delta runs behind its base
    /// image than the chain-length threshold, or more bytes of them than
    /// the byte threshold: what an earlier session can have left if it ran
    /// with other thresholds. Returns how many chains got a base image.
    pub fn consolidate_long_chains(&self) -> Result<usize, FlushError> {
        let table = self.stores.table;
        let config = &self.state.config;
        let over = |deltas: usize, bytes: usize| {
            deltas > config.max_chain_deltas || bytes > config.max_chain_bytes
        };
        let mut links: Vec<(PageId, Link)> = Vec::new();
        let mut long = Vec::new();
        let mut next = Some(PageId::MIN);
        while let Some(from) = next {
            links.clear();
            next = table.scan(from, &mut links);
            // The links of one page come one after another, oldest first.
            for chain in links.chunk_by(|one, other| one.0 == other.0) {
                let runs = chain.iter().rev().map_while(|(_, link)| match link.kind {
                    LinkKind::Delta(location) => Some(location.len as usize),
                    LinkKind::Base(_) | LinkKind::Free => None,
                });
                let (deltas, bytes) =
                    runs.fold((0, 0), |(deltas, bytes), len| (deltas + 1, bytes + len));
                if over(deltas, bytes) {
                    long.push(chain[0].0);
                }
            }
        }
        let mut consolidated = 0;
        for page in long {
            consolidated += usize::from(self.consolidate(page)?);
        }
        Ok(consolidated)
    }
}
