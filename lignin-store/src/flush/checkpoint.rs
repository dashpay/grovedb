//! Table checkpoints, and the log's files: what a checkpoint may cover, the
//! order in which the sample, the pages, the checkpoint file, the log and
//! the manifest are dealt with, and which log files can go afterwards
//! (`page-table.md` K1 to K15, `log-format.md` LG29).

use std::sync::atomic::Ordering;

use lignin_format::{segment::Compression, CommitVersion, PageId};

use super::{
    config::count,
    error::FlushError,
    page::PageKind,
    state::{lock, Checkpoints, Named},
    write_back::{Stores, WriteBack},
};
use crate::{
    segment::SegmentStoreError,
    table::{
        CheckpointInfo, CountSample, Link, LinkError, LinkKind, LogDurable, LogPlace,
        PageTableError, ValueCount,
    },
    wal::LogError,
};

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
        let place = in_force.and_then(CheckpointInfo::log_kept_from);
        let hold = self.stores.log.hold_from(place)?;
        let mut checkpoints = lock(&self.state.checkpoints);
        checkpoints.last_sample = place;
        checkpoints.in_force = Some(Named {
            covers: in_force.map_or(CommitVersion::EMPTY, CheckpointInfo::covers),
            place,
            _hold: hold,
        });
        Ok(())
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

    /// Keeps the log's files going: [`LogStore::roll`], when the file the
    /// log ends in is full, when an append to it failed, or when the
    /// session has no file yet. Returns whether the log turned to another
    /// file.
    ///
    /// After a failed append this is what makes the cut of CM9: the log
    /// takes no item until it has returned `Ok`.
    ///
    /// [`LogStore::roll`]: crate::wal::LogStore::roll
    pub fn roll_log(&self) -> Result<bool, FlushError> {
        let Stores { values, log, .. } = self.stores;
        let wanted = self.state.roll_wanted.load(Ordering::Acquire);
        if !wanted && !log.is_due() {
            return Ok(false);
        }
        let mark = values.applied();
        let turned = log.roll(&mut *self.manifest()?)?;
        values.mark_durable(mark)?;
        self.state.roll_wanted.store(false, Ordering::Release);
        Ok(turned)
    }

    /// Whether a table checkpoint is due: as many bytes were logged since
    /// the last one as the configuration says.
    pub fn checkpoint_due(&self) -> bool {
        self.state.logged.load(Ordering::Relaxed) >= self.state.config.checkpoint_log_bytes
    }

    /// The oldest version of which some change to a page is in no block,
    /// if there is one: what the pool lists as owed, and what was kept in
    /// the pool for readers and waits for the block of the next version of
    /// its page. A page that was freed since is not counted (T18).
    fn floor(&self) -> Option<CommitVersion> {
        let Stores { pool, table, .. } = self.stores;
        let folded = lock(&self.state.kept).oldest_folded();
        let mut freed = Vec::new();
        let waiting = folded.into_iter().filter_map(|(page, born)| {
            let newest = table.newest(page);
            if newest.is_some_and(|link| link.kind == LinkKind::Free && link.version.get() > born) {
                freed.push(page);
                return None;
            }
            CommitVersion::new(born)
        });
        let floor = waiting.chain(pool.oldest_unwritten()).min();
        if !freed.is_empty() {
            let mut kept = lock(&self.state.kept);
            freed.into_iter().for_each(|page| kept.forget(page));
        }
        floor
    }

    /// Whether every change of `page` up to the version `born`, which was
    /// owed when a sample was taken, is in a block now, or never will be
    /// because the page was freed.
    fn is_in_block(&self, page: PageId, born: CommitVersion) -> bool {
        let Stores { pool, table, .. } = self.stores;
        let Some(newest) = table.newest(page) else {
            // No link at all: the page version is gone from the pool as
            // well, or it still waits.
            return pool.get_version(page, born).is_none();
        };
        if newest.kind == LinkKind::Free && newest.version > born {
            return true;
        }
        let mut owed = Vec::new();
        pool.unwritten_pages(born, &mut owed);
        let folded = lock(&self.state.kept).folded(page);
        newest.version >= born
            && !owed.contains(&page)
            && folded.first().is_none_or(|&oldest| oldest > born.get())
    }

    /// Takes a table checkpoint and has the manifest name it, and then
    /// takes out of the log the files that nothing needs any more.
    ///
    /// The steps, in the order `page-table.md` 4.6 asks for:
    ///
    /// 1. The log is rolled if that is due, and a pass writes back what it
    ///    can.
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
    pub fn checkpoint(&self) -> Result<Checkpointed, FlushError> {
        let Stores {
            values,
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
        self.pass()?;

        let in_force = state.in_force.as_ref().map(|named| named.covers);
        let step = values.step()?;
        let latest = versions.latest();
        let written = match self.floor() {
            Some(floor) => CommitVersion::new(floor.get().saturating_sub(1)).unwrap_or(latest),
            None => latest,
        };
        let covers = written.min(latest).max(in_force.unwrap_or(CommitVersion::EMPTY));
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
        let named = Named {
            covers,
            place: Some(place),
            _hold: hold,
        };
        let durable = match self.sync_log() {
            Ok(durable) => durable,
            Err(error) => {
                let _ = checkpoints.discard(info, &*self.manifest()?);
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
            count(&self.state.counters.checkpoints, 1);
        } else {
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
        let scratch = std::mem::take(&mut files.scratch);
        let written = super::page::write_image(&scratch, &mut files.image);
        files.scratch = scratch;
        if written.map_err(FlushError::page(page))? {
            return Err(FlushError::PendingValue {
                page,
                born: newest.version,
            });
        }
        let image = std::mem::take(&mut files.image);
        let location = self.append(
            files,
            page,
            lignin_format::segment::BlockContent::Base,
            Compression::Lz4,
            &image,
        );
        files.image = image;
        let location = location?;
        let counters = &self.state.counters;
        match table.consolidate(page, through, location) {
            Ok(version) => {
                count(&counters.base_blocks, 1);
                count(&counters.base_bytes, files.image.len() as u64);
                count(&counters.stored_bytes, u64::from(location.len));
                count(&counters.consolidations, 1);
                files.untrimmed.insert(page, version);
                Ok(true)
            }
            Err(LinkError::NotInChain { .. }) => {
                count(&counters.links_refused, 1);
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
            let mut chain: Option<(PageId, usize, usize)> = None;
            for &(page, link) in &links {
                let (deltas, bytes) = match chain {
                    Some((same, deltas, bytes)) if same == page => (deltas, bytes),
                    Some((other, deltas, bytes)) => {
                        if over(deltas, bytes) {
                            long.push(other);
                        }
                        (0, 0)
                    }
                    None => (0, 0),
                };
                chain = Some(match link.kind {
                    LinkKind::Delta(location) => (page, deltas + 1, bytes + location.len as usize),
                    LinkKind::Base(_) | LinkKind::Free => (page, 0, 0),
                });
            }
            if let Some((page, deltas, bytes)) = chain
                && over(deltas, bytes)
            {
                long.push(page);
            }
        }
        let mut consolidated = 0;
        for page in long {
            consolidated += usize::from(self.consolidate(page)?);
        }
        Ok(consolidated)
    }
}
