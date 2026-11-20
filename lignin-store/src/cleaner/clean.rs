//! The cleaner's view of a store, and its pass over the segments: the
//! accounting, the trimming of chains, the moving of live blocks, and the
//! deletion of the segments that nothing points into any more.

use std::collections::{BTreeMap, BTreeSet};

use lignin_format::{
    manifest::FileState,
    segment::{BlockContent, Compression, DATA_START},
    CommitVersion, FileId, PageId,
};

use super::{
    config::{count, CleanerConfig},
    error::CleanerError,
    space::{is_at_least, is_over, trimmable, Live, SegmentSpace, Space},
    state::{lock, CleanerState, RetiredSegment, Seen, Work},
};
use crate::{
    flush::{Checkpointed, Stores, WriteBack},
    segment::{BlockLocation, SegmentStoreError, SegmentWriter},
    stats::Stats,
    table::{Link, LinkError, PageTable},
};

/// What one pass over the segments did ([`Cleaner::pass`]).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Pass {
    /// Whether the pass read the page table through. A pass that finds the
    /// store as an earlier pass left it, with nothing to wait for, does
    /// not.
    pub scanned: bool,
    /// Links to blocks that were trimmed out of chains. The free link of
    /// a page that goes with its chain is not counted.
    pub links_trimmed: u64,
    /// Blocks that were moved to another segment.
    pub blocks_moved: u64,
    /// The bytes their copies take.
    pub bytes_moved: u64,
    /// Blocks that were copied and whose link was gone by then.
    pub blocks_lost: u64,
    /// The bytes their copies take. They are written like the copies of
    /// moved blocks, and count against the throttle as those do.
    pub bytes_lost: u64,
    /// Segments that the pass retired: the page table points into them no
    /// more.
    pub retired: usize,
    /// Segments that the pass took out of the store.
    pub dropped: usize,
    /// Segments that still wait to be deleted when the pass ends.
    pub waiting: usize,
    /// Table checkpoints that the pass asked for and that were installed.
    pub checkpoints: u64,
    /// A table checkpoint the pass asked for that was put off: a write
    /// transaction holds back a page that has to be in it. The segments
    /// that wait for it go on waiting.
    pub checkpoint: Option<Checkpointed>,
    /// The segments that were being filled and that the pass had sealed:
    /// write-back's, the cleaner's own, or both.
    pub sealed: Vec<FileId>,
    /// Whether the throttle ended the moving of blocks before the dead
    /// space was within its target: the next pass goes on.
    pub more: bool,
}

impl Pass {
    /// Whether the pass moved anything on.
    pub fn progressed(&self) -> bool {
        self.links_trimmed + self.blocks_moved + self.blocks_lost != 0
            || self.retired + self.dropped != 0
            || !self.sealed.is_empty()
            || self.checkpoints != 0
    }
}

/// How many rounds a pass makes at most: each can end with a table
/// checkpoint.
const MAX_ROUNDS: usize = 8;

/// What became of a block that was copied.
enum Moved {
    /// Its link points at the copy.
    Linked(BlockLocation),
    /// Its link was gone: the copy is dead space.
    Lost(BlockLocation),
}

/// How many chains are trimmed under one hold of write-back's lock on its
/// files. After a large drop there are many, and a pass of write-back,
/// which committers can be waiting for, waits for that lock.
const TRIM_CHAINS: usize = 1024;

/// A block is moved with its payload as it is, and is read back under the
/// format version of the segment that took it. That is right while every
/// segment is of one version. Whoever adds a version says here what a
/// move does with a block of an older one, and until then this does not
/// compile.
const _: () = match lignin_format::FormatVersion::CURRENT {
    lignin_format::FormatVersion::V1 => (),
};

/// Calls `each` with every link of the page table, one step of
/// [`PageTable::scan`] at a time: the links of one page come one after
/// another, oldest first, and no chain is cut in two.
pub(super) fn scan_table(table: &PageTable, mut each: impl FnMut(&[(PageId, Link)])) {
    let mut links = Vec::new();
    let mut next = Some(PageId::MIN);
    while let Some(from) = next {
        links.clear();
        next = table.scan(from, &mut links);
        each(&links);
    }
}

/// The cleaner of one open store: write-back's view of the stores, and
/// the cleaner's own state, as one handle that can be copied to every
/// thread. See the [module documentation](super).
#[derive(Debug, Clone, Copy)]
pub struct Cleaner<'s> {
    pub(super) write_back: WriteBack<'s>,
    pub(super) state: &'s CleanerState,
    pub(super) stats: Option<&'s Stats>,
}

impl<'s> Cleaner<'s> {
    /// The cleaner of the store that `write_back` writes back, with
    /// `state` as what it keeps between passes.
    ///
    /// It takes no lock, so a view can be made wherever one is needed.
    pub fn new(write_back: WriteBack<'s>, state: &'s CleanerState) -> Self {
        Self {
            write_back,
            state,
            stats: None,
        }
    }

    /// The same cleaner, which counts the bytes it writes in `stats`
    /// ([`Stats::count_cleaner_bytes`]).
    pub fn with_stats(mut self, stats: &'s Stats) -> Self {
        self.stats = Some(stats);
        self
    }

    /// The state this handle works on.
    pub fn state(&self) -> &'s CleanerState {
        self.state
    }

    /// Write-back's view of the store this handle works on.
    pub fn write_back(&self) -> WriteBack<'s> {
        self.write_back
    }

    pub(super) fn stores(&self) -> Stores<'s> {
        self.write_back.stores()
    }

    pub(super) fn wrote(&self, bytes: u64) {
        if let Some(stats) = self.stats {
            stats.count_cleaner_bytes(bytes);
        }
    }

    /// What the segments hold now: a read of the whole page table, which
    /// changes nothing. It waits for a pass that is under way.
    pub fn space(&self) -> Result<Space, CleanerError> {
        let mut work = lock(&self.state.work);
        let (mut space, _) = self.read_table(&mut work, None)?;
        mark_retired(&mut space, &work);
        Ok(space)
    }

    /// One pass over the segments. In this order:
    ///
    /// 1. The page table is read through: what each segment holds that a
    ///    link points to. The chains that hold links no reader needs any
    ///    more are trimmed.
    /// 2. The segments that the table points into no more are retired.
    /// 3. The retired segments that no running operation can still read
    ///    from, and that the table checkpoint in force does not point
    ///    into, are taken out of the store and deleted. A table checkpoint
    ///    is asked for if the segments that only the checkpoint in force
    ///    keeps hold too much.
    /// 4. A segment that is being filled is sealed if too much of it is
    ///    dead, or if it has become a large part of the store.
    /// 5. While the dead space is above its target, the live blocks of the
    ///    segments with the largest share of dead bytes are moved to the
    ///    cleaner's own segment: as many segments as can wait for one
    ///    table checkpoint, and as many bytes as the throttle allows. A
    ///    segment that is left with no live block is retired, and step 3
    ///    is made again.
    ///
    /// If step 5 moved blocks, the pass goes round again, a few times at
    /// most: what was emptied may wait for a table checkpoint, and a round
    /// that finds the dead space above its target still has one taken.
    ///
    /// One pass runs at a time, and a second call waits for the first. A
    /// pass that finds the store as the pass before left it, with no
    /// segment waiting, does not read the page table: it looks at the
    /// counts of the table and of write-back, the oldest pin, and which
    /// segments there are and which have a writer.
    ///
    /// Whatever it returns, what it did stands, and it can be called
    /// again.
    pub fn pass(&self) -> Result<Pass, CleanerError> {
        let mut work = lock(&self.state.work);
        let counters = &self.state.counters;
        count(&counters.passes, 1);
        let mut pass = Pass::default();
        let ended = self.pass_with(&mut work, &mut pass);
        pass.waiting = work.retired.len();
        if ended.is_err() {
            count(&counters.errors, 1);
        }
        ended.map(|()| pass)
    }

    /// What tells a pass whether the store is as the pass before left it.
    fn seen(&self) -> Seen {
        let Stores {
            table,
            segments,
            versions,
            ..
        } = self.stores();
        let written = self.write_back.state().stats();
        let extents = segments.extents();
        Seen {
            links: table.links(),
            pages: table.pages(),
            blocks_written: written.base_blocks + written.delta_blocks + written.links_refused,
            oldest_pin: versions.oldest_pin(),
            segments: extents.len(),
            writers: extents.iter().filter(|extent| extent.has_writer).count(),
        }
    }

    fn pass_with(&self, work: &mut Work, pass: &mut Pass) -> Result<(), CleanerError> {
        let seen = self.seen();
        if work.idle_at == Some(seen) && work.retired.is_empty() {
            return Ok(());
        }
        work.idle_at = None;
        // A round moves as much as can wait for one table checkpoint, and
        // has the checkpoint taken. While that sets segments free and the
        // dead space is still above its target, the pass goes on.
        // The first damage the rounds find is put aside and the pass goes
        // on. It is what the pass ends with, whatever fails afterwards:
        // the segment is left alone for the rest of the session, and no
        // later pass of it names the block again.
        let mut failed = None;
        for _ in 0..MAX_ROUNDS {
            let more = match self.round(work, pass, &mut failed) {
                Ok(more) => more,
                Err(error) => return Err(failed.unwrap_or(error)),
            };
            if !more || pass.more {
                break;
            }
        }
        failed.map_or(Ok(()), Err)?;
        let waits = !work.retired.is_empty() || work.trim_bound.waiting() > 0;
        if !pass.progressed() && !pass.more && !waits {
            work.idle_at = Some(seen);
        }
        Ok(())
    }

    /// One round of a pass. Returns whether another one may move more:
    /// this one moved blocks. An error of a move that leaves the rest of
    /// the round to be made is put into `failed`.
    fn round(
        &self,
        work: &mut Work,
        pass: &mut Pass,
        failed: &mut Option<CleanerError>,
    ) -> Result<bool, CleanerError> {
        let config = &self.state.config;
        let counters = &self.state.counters;
        let trim_at = work.trim_bound.ask(self.stores().versions);
        let (mut space, trim) = self.read_table(work, trim_at)?;
        pass.scanned = true;
        count(&counters.scans, 1);
        for (pages, trim_at) in trim
            .chunks(TRIM_CHAINS)
            .zip(std::iter::repeat(trim_at).flatten())
        {
            let removed = self.write_back.trim_chains(pages, trim_at);
            let bytes: u64 = removed
                .iter()
                .filter_map(Link::location)
                .map(|location| u64::from(location.len))
                .sum();
            for link in &removed {
                take_off(&mut space, link);
            }
            pass.links_trimmed += removed.len() as u64;
            count(&counters.links_trimmed, removed.len() as u64);
            count(&counters.bytes_trimmed, bytes);
        }

        let empty: Vec<(FileId, u64)> = space
            .segments
            .iter()
            .filter(|segment| !segment.has_writer && segment.live_blocks == 0)
            .filter(|segment| !work.retired.contains_key(&segment.file))
            .map(|segment| (segment.file, segment.block_bytes))
            .collect();
        for (file, block_bytes) in empty {
            self.retire(work, file, block_bytes, pass);
        }
        // What is retired now, or was when the table was read, is not
        // counted among what the cleaner works on, whatever becomes of it
        // in this round.
        let out: BTreeSet<FileId> = work.retired.keys().copied().collect();
        let all = space.block_bytes();
        let mut waiting = self.reclaim_with_checkpoint(work, pass, all, false)?;

        // A segment is not cleaned while it is written. One that is being
        // filled, by write-back or with the blocks the cleaner moved, is
        // sealed once too much of it is dead, and once it is so large a
        // part of the store that moving what lives in it would take too
        // much room at once.
        let own = work.segment.as_ref().map(SegmentWriter::file_id);
        let to_seal: Vec<FileId> = space
            .segments
            .iter()
            .filter(|segment| segment.has_writer)
            .filter(|segment| segment.block_bytes >= config.min_segment_len)
            .filter(|segment| {
                is_at_least(segment.dead_bytes(), all, config.seal_dead_percent)
                    || is_at_least(segment.block_bytes, all, config.max_segment_percent)
            })
            .map(|segment| segment.file)
            .collect();
        for file in to_seal {
            let sealed = match Some(file) == own {
                true => {
                    let writer = work.segment.take().expect("the cleaner's segment");
                    self.seal(work, writer)?;
                    true
                }
                false => self.write_back.seal_segment(file)?,
            };
            if sealed {
                pass.sealed.push(file);
                count(&counters.segments_sealed, 1);
            }
        }

        // With a segment that cannot be made durable no table checkpoint
        // is installed in this session, so a copy would never let its
        // block go: nothing is moved.
        let durable = work.failed.is_empty() && !self.write_back.state().segments_not_durable();
        let mut chosen = match durable {
            true => victims(&space, &out, &work.unreadable, config, waiting),
            false => Chosen::default(),
        };
        if chosen.no_room {
            // The dead space is above its target, and what waits to be
            // deleted leaves no room for the segment that is most worth
            // it: a table checkpoint sets free what waits for one. What
            // waits for a running operation stays, and a smaller segment
            // is taken then.
            waiting = self.reclaim_with_checkpoint(work, pass, all, true)?;
            chosen = victims(&space, &out, &work.unreadable, config, waiting);
        }
        mark_retired(&mut space, work);
        *lock(&self.state.space) = space.clone();
        if chosen.segments.is_empty() {
            return Ok(false);
        }
        let moved = pass.blocks_moved;
        self.move_out(work, &space, &chosen.segments, pass, failed)?;
        self.reclaim_with_checkpoint(work, pass, all, false)?;
        // The next round reads the table again. If the dead space is
        // still above its target then, it has a table checkpoint taken
        // for what this round emptied, and goes on.
        Ok(pass.blocks_moved > moved)
    }

    /// Takes out of the store the retired segments that nothing needs any
    /// more, and has a table checkpoint taken if the segments that only
    /// the checkpoint in force keeps hold too much, or if any wait for one
    /// and one is `wanted`. Once one was put off, none is asked for again
    /// in the pass.
    ///
    /// Returns the bytes of the blocks of the segments that still wait to
    /// be deleted, whatever they wait for. One that waits for a running
    /// operation takes its room as one that waits for a checkpoint does:
    /// until it is deleted, the store holds twice what was moved out of
    /// it.
    fn reclaim_with_checkpoint(
        &self,
        work: &mut Work,
        pass: &mut Pass,
        all: u64,
        wanted: bool,
    ) -> Result<u64, CleanerError> {
        let config = &self.state.config;
        let counters = &self.state.counters;
        let kept = self.reclaim(work, pass)?;
        let due = is_at_least(kept, all, config.checkpoint_dead_percent);
        let ask = (due || (wanted && kept != 0))
            && pass.checkpoint.is_none()
            && !self.write_back.state().segments_not_durable();
        if ask {
            match self.write_back.checkpoint() {
                Ok(Checkpointed::Installed { .. }) => {
                    count(&counters.checkpoints, 1);
                    pass.checkpoints += 1;
                    self.reclaim(work, pass)?;
                }
                Ok(put_off) => {
                    count(&counters.checkpoints_missed, 1);
                    pass.checkpoint = Some(put_off);
                }
                Err(error) => {
                    count(&counters.checkpoints_missed, 1);
                    return Err(error.into());
                }
            }
        }
        Ok(work
            .retired
            .values()
            .map(|retired| retired.block_bytes)
            .sum())
    }

    /// Reads the page table through: what each segment holds that a link
    /// points to, and the pages whose chains a trim at `trim_at` would
    /// shorten.
    fn read_table(
        &self,
        work: &mut Work,
        trim_at: Option<CommitVersion>,
    ) -> Result<(Space, Vec<PageId>), CleanerError> {
        let Stores {
            table,
            segments,
            versions,
            ..
        } = self.stores();
        // The segments are looked at first. One that has no writer now
        // takes no more blocks, and the link of every block it holds was
        // pushed before its writer went: every link into it that the read
        // does not find is gone for good.
        let extents = segments.extents();
        let latest = versions.latest();
        let oldest = versions.oldest_pin();

        let mut live = Live::default();
        let mut trim = Vec::new();
        let mut chain: Vec<Link> = Vec::new();
        scan_table(table, |links| {
            for of_page in links.chunk_by(|one, other| one.0 == other.0) {
                chain.clear();
                chain.extend(of_page.iter().map(|(_, link)| *link));
                if trim_at.is_some_and(|at| trimmable(&chain, at) > 0) {
                    trim.push(of_page[0].0);
                }
                live.chain(&chain, oldest, latest);
            }
        });

        work.blocks_end
            .retain(|file, _| extents.iter().any(|extent| extent.file == *file));
        let mut space = Space {
            segments: Vec::with_capacity(extents.len()),
            oldest_pin: Some(oldest),
        };
        for extent in extents {
            let blocks_end = match (extent.state, work.blocks_end.get(&extent.file)) {
                (FileState::Sealed, Some(end)) => *end,
                (FileState::Sealed, None) => match segments.blocks_end(extent.file) {
                    Ok(end) => {
                        work.blocks_end.insert(extent.file, end);
                        end
                    }
                    // A footer that cannot be read stops no reader, and
                    // is not to stop the cleaner: the end of the file
                    // stands for the end of the blocks, which counts the
                    // footer among what is dead. It is read again by the
                    // next pass.
                    Err(_) => extent.end,
                },
                _ => extent.end,
            };
            let (live_bytes, live_blocks, pinned_bytes) = live.of(extent.file);
            space.segments.push(SegmentSpace {
                file: extent.file,
                state: extent.state,
                has_writer: extent.has_writer,
                block_bytes: blocks_end.saturating_sub(DATA_START),
                live_bytes,
                live_blocks,
                pinned_bytes,
                retired: false,
            });
        }
        Ok((space, trim))
    }

    /// Retires the segment `file`, which the page table points into no
    /// more and never will again. The epoch is started now, after that was
    /// found (`transactions.md` PN6): an operation that read a location in
    /// the segment holds a guard of an earlier epoch.
    fn retire(&self, work: &mut Work, file: FileId, block_bytes: u64, pass: &mut Pass) {
        let epoch = self.stores().versions.next_epoch();
        work.retired
            .insert(file, RetiredSegment { epoch, block_bytes });
        pass.retired += 1;
        count(&self.state.counters.segments_retired, 1);
    }

    /// Moves the live blocks of `victims` to the cleaner's segment, in the
    /// order of the victims, until the throttle ends the pass. A victim
    /// that is left with no live block is retired. The first damaged block
    /// it meets is put into `failed`, unless `failed` holds an error
    /// already, and the move goes on with the other victims.
    fn move_out(
        &self,
        work: &mut Work,
        space: &Space,
        victims: &[FileId],
        pass: &mut Pass,
        failed: &mut Option<CleanerError>,
    ) -> Result<(), CleanerError> {
        let table = self.stores().table;
        let counters = &self.state.counters;
        let order: BTreeMap<FileId, usize> = victims
            .iter()
            .enumerate()
            .map(|(at, file)| (*file, at))
            .collect();
        // A second read of the page table, for the links into the victims.
        // Each of them takes no more blocks, so a link that this read does
        // not find is gone for good.
        let mut blocks: Vec<(usize, BlockLocation, PageId)> = Vec::new();
        scan_table(table, |links| {
            for (page, link) in links {
                let found = link
                    .location()
                    .and_then(|location| Some((*order.get(&location.file)?, location)));
                if let Some((at, location)) = found {
                    blocks.push((at, location, *page));
                }
            }
        });
        blocks.sort_unstable_by_key(|(at, location, _)| (*at, location.offset));

        let mut left = vec![0u64; victims.len()];
        for (at, ..) in &blocks {
            left[*at] += 1;
        }
        let mut buf = Vec::new();
        for (at, from, page) in blocks {
            if work.unreadable.contains(&from.file) {
                continue;
            }
            if pass.bytes_moved + pass.bytes_lost >= self.state.config.max_pass_bytes {
                pass.more = true;
                break;
            }
            match self.move_block(work, page, from, &mut buf) {
                Ok(Moved::Linked(to)) => {
                    pass.blocks_moved += 1;
                    pass.bytes_moved += u64::from(to.len);
                    count(&counters.blocks_moved, 1);
                    count(&counters.bytes_moved, u64::from(to.len));
                }
                Ok(Moved::Lost(to)) => {
                    pass.blocks_lost += 1;
                    pass.bytes_lost += u64::from(to.len);
                    count(&counters.blocks_lost, 1);
                    count(&counters.bytes_lost, u64::from(to.len));
                }
                // A live block that is damaged is damage that its readers
                // meet too. The segment keeps its link, so it is never
                // deleted, and moving the rest of it would gain nothing.
                Err(error) if error.is_damage() => {
                    work.unreadable.insert(from.file);
                    count(&counters.segments_unreadable, 1);
                    failed.get_or_insert(error);
                    continue;
                }
                Err(error) => return Err(error),
            }
            left[at] -= 1;
        }
        for (at, file) in victims.iter().enumerate() {
            if left[at] == 0 && !work.unreadable.contains(file) {
                let block_bytes = space.segment(*file).map_or(0, |held| held.block_bytes);
                self.retire(work, *file, block_bytes, pass);
            }
        }
        Ok(())
    }

    /// Copies the block at `from`, which a link of `page` points to, to
    /// the cleaner's segment, and points the link at the copy
    /// (`transactions.md` CL2). If the link is gone by then, because the
    /// block was superseded and trimmed while it was copied, nothing is
    /// switched and the copy is dead (CL4).
    fn move_block(
        &self,
        work: &mut Work,
        page: PageId,
        from: BlockLocation,
        buf: &mut Vec<u8>,
    ) -> Result<Moved, CleanerError> {
        let Stores {
            table,
            segments,
            pool,
            ..
        } = self.stores();
        // No block holds more than a page: an image is one, and a run is
        // no longer (`flush::max_run_len`).
        let block = segments
            .read(from, pool.frame_len(), buf)
            .map_err(CleanerError::block(page, from))?;
        let (content, compression) = (block.content(), block.compression());
        let payload = block.payload().map_err(CleanerError::payload(page, from))?;
        let to = self.append(work, page, content, compression, &payload)?;
        self.wrote(u64::from(to.len));
        #[cfg(test)]
        if let Some(meanwhile) = lock(&self.state.at_switch).as_mut() {
            (meanwhile.0)(page, from);
        }
        match table.relocate(page, from, to) {
            Ok(()) => Ok(Moved::Linked(to)),
            Err(LinkError::NotInChain { .. }) => Ok(Moved::Lost(to)),
            Err(source) => Err(CleanerError::Link {
                page,
                from,
                to,
                source,
            }),
        }
    }

    /// Appends one block to the segment that takes the moved blocks, and
    /// starts a segment first if there is none or the one there is is
    /// full.
    fn append(
        &self,
        work: &mut Work,
        page: PageId,
        content: BlockContent,
        compression: Compression,
        payload: &[u8],
    ) -> Result<BlockLocation, CleanerError> {
        let segments = self.stores().segments;
        let segment_len = self.state.config.segment_len;
        if let Some(full) = work.segment.take_if(|writer| writer.end() >= segment_len) {
            self.seal(work, full)?;
        }
        if work.segment.is_none() {
            let writer = segments.create()?;
            segments.commit(&mut *self.write_back.manifest()?)?;
            work.segment = Some(writer);
        }
        let writer = work.segment.as_mut().expect("a segment was just made");
        match writer.append(page, content, compression, payload) {
            Ok(location) => Ok(location),
            Err(SegmentStoreError::SegmentFull { .. }) => {
                let full = work.segment.take().expect("the segment that is full");
                self.seal(work, full)?;
                self.append(work, page, content, compression, payload)
            }
            Err(error) => {
                // What was appended stays readable, and a sync of the
                // store still makes it durable. The next block goes to
                // another segment.
                let writer = work.segment.take().expect("the segment that failed");
                self.segment_failed(work, writer)?;
                Err(error.into())
            }
        }
    }

    /// Seals a segment that takes no more blocks. If that fails, the
    /// segment is one that failed.
    pub(super) fn seal(
        &self,
        work: &mut Work,
        mut writer: SegmentWriter,
    ) -> Result<(), CleanerError> {
        if let Err(error) = writer.seal() {
            self.segment_failed(work, writer)?;
            return Err(error.into());
        }
        Ok(())
    }

    /// Ends the use of a segment that an append or a seal failed. What was
    /// appended to it is synced. If that sync fails, the writer is kept
    /// and not dropped: dropping it would cut the segment at what was
    /// synced, and the blocks behind that, which chains point to, could no
    /// longer be read.
    fn segment_failed(
        &self,
        work: &mut Work,
        mut writer: SegmentWriter,
    ) -> Result<(), CleanerError> {
        let Err(source) = writer.sync() else {
            return Ok(());
        };
        let file = writer.file_id();
        work.failed.push(writer);
        Err(CleanerError::SegmentNotDurable { file, source })
    }

    /// Takes out of the store the retired segments that nothing needs any
    /// more (`transactions.md` CL3), and has them deleted:
    ///
    /// - every operation that was running when the segment was retired
    ///   has ended (PN6);
    /// - neither the table checkpoint in force nor one whose installation
    ///   is in doubt points into it (`page-table.md` K6).
    ///
    /// A checkpoint of the database that is being created needs the files
    /// of the manifest it sampled until it has linked them (CK3). It
    /// holds the manifest's lock for that, and under that lock the drop
    /// is made here, the manifest that names the segment no more is
    /// written, and the file is deleted.
    fn reclaim(&self, work: &mut Work, pass: &mut Pass) -> Result<u64, CleanerError> {
        let Stores {
            segments, versions, ..
        } = self.stores();
        let counters = &self.state.counters;
        if work.retired.is_empty() {
            return Ok(0);
        }
        let oldest_guard = versions.oldest_guard();
        let unread: Vec<FileId> = work
            .retired
            .iter()
            .filter(|(_, retired)| retired.epoch <= oldest_guard)
            .map(|(file, _)| *file)
            .collect();
        if unread.is_empty() {
            return Ok(0);
        }
        // This waits for a table checkpoint that is being taken. One that
        // is begun afterwards copies a page table that points into none of
        // these segments.
        let Some(in_checkpoint) = self.write_back.state().checkpoint_segments() else {
            return Ok(0);
        };
        let (named, free): (Vec<FileId>, Vec<FileId>) = unread
            .into_iter()
            .partition(|file| in_checkpoint.contains(file));
        let kept: u64 = named
            .iter()
            .filter_map(|file| work.retired.get(file))
            .map(|retired| retired.block_bytes)
            .sum();
        if free.is_empty() {
            return Ok(kept);
        }

        // The drop, the manifest that no longer names the segment and the
        // deletion of the file are one step under the manifest's lock,
        // which a checkpoint of the database holds while it samples the
        // manifest and links the files it names.
        let mut manifest = self.write_back.manifest()?;
        let (mut dropped, mut failed) = (0u64, None);
        for file in free {
            match segments.drop_segment(file) {
                Ok(()) => {
                    dropped += 1;
                    if let Some(retired) = work.retired.remove(&file) {
                        count(&counters.bytes_dropped, retired.block_bytes);
                    }
                }
                // The store has it no more: an earlier pass dropped it and
                // failed before it could forget it.
                Err(SegmentStoreError::UnknownSegment { .. }) => {
                    work.retired.remove(&file);
                }
                // What was dropped before it is still taken out of the
                // manifest.
                Err(error) => {
                    failed = Some(error);
                    break;
                }
            }
        }
        if dropped != 0 {
            pass.dropped += dropped as usize;
            count(&counters.segments_dropped, dropped);
            match segments.commit(&mut manifest) {
                // The new manifest is in force, and a later replacement
                // deletes the file.
                Ok(()) | Err(SegmentStoreError::NotDeleted { .. }) => {}
                Err(error) => return Err(error.into()),
            }
        }
        failed.map_or(Ok(kept), |error| Err(error.into()))
    }

    /// What a session does with the cleaner before it ends: the segment
    /// and the value-log file that took the copies are closed. Call it
    /// once the background loop has returned ([`Self::stop`]) and before
    /// write-back's own [`WriteBack::finish`], whose manifest then names
    /// them as they are.
    pub fn finish(&self) -> Result<(), CleanerError> {
        let mut work = lock(&self.state.work);
        let sealed = match work.segment.take() {
            Some(writer) => self.seal(&mut work, writer),
            None => Ok(()),
        };
        let finished = match work.values.take() {
            Some(writer) => writer.finish().map(drop).map_err(CleanerError::from),
            None => Ok(()),
        };
        sealed.and(finished)
    }
}

/// Takes a link that a trim removed off what `space` counted as live.
fn take_off(space: &mut Space, link: &Link) {
    let Some(location) = link.location() else {
        return;
    };
    let at = space
        .segments
        .binary_search_by_key(&location.file, |segment| segment.file);
    if let Ok(at) = at {
        let segment = &mut space.segments[at];
        segment.live_bytes = segment.live_bytes.saturating_sub(u64::from(location.len));
        segment.live_blocks = segment.live_blocks.saturating_sub(1);
        segment.pinned_bytes = segment.pinned_bytes.min(segment.live_bytes);
    }
}

fn mark_retired(space: &mut Space, work: &Work) {
    for segment in &mut space.segments {
        segment.retired = work.retired.contains_key(&segment.file);
    }
}

/// The segments a round moves the live blocks out of.
#[derive(Debug, Default)]
pub(super) struct Chosen {
    pub(super) segments: Vec<FileId>,
    /// Whether the segment that is most worth it was passed over, because
    /// what waits to be deleted leaves no room for it.
    pub(super) no_room: bool,
}

/// The segments whose live blocks are to be moved, the one with the
/// largest share of dead bytes first: as many as it takes to bring the
/// dead bytes of the segments that are not retired down to the target, and
/// no more than can wait for one table checkpoint.
///
/// Moving the live blocks out of a segment and deleting it takes its dead
/// bytes out of both sums: the copies are as long as what they copy. Until
/// the segment is deleted, the store holds its blocks twice, and a segment
/// that the table checkpoint in force points into is deleted only after
/// the next one. So what is chosen, with the `waiting` bytes of the
/// segments that wait to be deleted already, is no more than the share at
/// which a checkpoint is asked for
/// ([`CleanerConfig::checkpoint_dead_percent`]). With nothing waiting, one
/// segment is chosen whatever its size. A segment that does not fit is
/// passed over for one further down that does, and the caller is told if
/// that was the first one ([`Chosen::no_room`]): a table checkpoint may
/// make the room for it.
///
/// `out` are the segments that are retired, or were when `space` was
/// read. They are left out of the sums, and so are the segments that
/// cannot be read: what is dead in them stays, whatever is moved out of
/// the others.
pub(super) fn victims(
    space: &Space,
    out: &BTreeSet<FileId>,
    unreadable: &BTreeSet<FileId>,
    config: &CleanerConfig,
    waiting: u64,
) -> Chosen {
    let kept = |segment: &&SegmentSpace| {
        !out.contains(&segment.file) && !unreadable.contains(&segment.file)
    };
    let mut dead: u64 = space
        .segments
        .iter()
        .filter(kept)
        .map(SegmentSpace::dead_bytes)
        .sum();
    let mut all: u64 = space
        .segments
        .iter()
        .filter(kept)
        .map(|segment| segment.block_bytes)
        .sum();
    let total = space.block_bytes();
    let mut candidates: Vec<&SegmentSpace> = space
        .segments
        .iter()
        .filter(kept)
        .filter(|segment| !segment.has_writer && segment.live_blocks != 0)
        .filter(|segment| segment.dead_bytes() != 0)
        .collect();
    candidates.sort_by(|one, other| {
        let one_share = u128::from(one.dead_bytes()) * u128::from(other.block_bytes);
        let other_share = u128::from(other.dead_bytes()) * u128::from(one.block_bytes);
        other_share.cmp(&one_share).then(one.file.cmp(&other.file))
    });
    let mut chosen = Chosen::default();
    let mut waits = waiting;
    for segment in candidates {
        if !is_over(dead, all, config.target_dead_percent) {
            break;
        }
        let with_it = waits.saturating_add(segment.block_bytes);
        if waits != 0 && is_over(with_it, total, config.checkpoint_dead_percent) {
            // It does not fit beside what waits. A smaller one further
            // down may.
            chosen.no_room |= chosen.segments.is_empty();
            continue;
        }
        chosen.segments.push(segment.file);
        dead -= segment.dead_bytes();
        all -= segment.dead_bytes();
        waits = with_it;
    }
    chosen
}
