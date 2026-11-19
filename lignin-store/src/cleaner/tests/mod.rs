//! Tests of the cleaner, on the store of write-back's tests: commits that
//! write one log group each, a write-back that writes their pages to
//! segments, and a model of what every version of every page says.

mod account;
mod crash;
mod deleting;
mod errors;
mod faults;
mod moving;
mod threads;
mod values;

use std::collections::BTreeMap;

use lignin_format::{
    manifest::FileState, tree_page::TreePage, value_log::ValuePointer, wal::ValueMove,
    CommitVersion, FileId, PageId, SlotId,
};

use super::*;
use crate::{
    alloc::Pins,
    flush::{
        tests::{hash, inline, insert, is_flat, long, noise, page, version, Db, Op},
        Checkpointed, FlushConfig,
    },
    load::FRAME_FORMAT,
    segment::BlockLocation,
    table::Link,
    values::{OwnersError, PointerRewrite, Refusal, Rewrite, ValueOwners},
};

/// A write-back that seals a segment after a few blocks, so that a store
/// of a few pages has several.
pub(super) fn small_segments() -> FlushConfig {
    FlushConfig {
        max_chain_deltas: 2,
        segment_len: 300,
        ..FlushConfig::new(Db::SIZE)
    }
}

/// A cleaner that leaves nothing dead where it can help it: it moves
/// blocks whenever a segment holds a dead byte, asks for a table
/// checkpoint whenever a segment waits for one, and has the segment that
/// write-back fills sealed whenever something in it is dead.
pub(super) fn eager() -> CleanerConfig {
    CleanerConfig {
        target_dead_percent: 0,
        checkpoint_dead_percent: 0,
        seal_dead_percent: 0,
        min_segment_len: 0,
        max_pass_bytes: u64::MAX,
        segment_len: 300,
        value_dead_percent: 0,
        idle: std::time::Duration::from_millis(1),
    }
}

/// A cleaner that moves blocks as [`eager`] does, and neither asks for a
/// table checkpoint nor has a segment sealed.
pub(super) fn moving_only() -> CleanerConfig {
    CleanerConfig {
        checkpoint_dead_percent: 101,
        seal_dead_percent: 101,
        ..eager()
    }
}

/// The bytes of all blocks of the segment `file`, and how many there are,
/// as a walk over the segment finds them.
pub(super) fn blocks_of(db: &Db, file: FileId) -> (u64, u64) {
    let mut walk = db.segments.walk(file, db.size.get()).expect("a walk");
    let (mut bytes, mut blocks) = (0, 0);
    while let Some((location, _)) = walk.next_block().expect("a block") {
        bytes += u64::from(location.len);
        blocks += 1;
    }
    (bytes, blocks)
}

/// Every link of the page table that points to a block, with its page.
pub(super) fn links_of(db: &Db) -> Vec<(PageId, Link)> {
    let mut all = Vec::new();
    let mut links = Vec::new();
    let mut next = Some(PageId::MIN);
    while let Some(from) = next {
        links.clear();
        next = db.table.scan(from, &mut links);
        all.extend(links.iter().filter(|(_, link)| link.location().is_some()));
    }
    all
}

/// The bytes and the number of the blocks of each segment that a link
/// points to.
pub(super) fn linked(db: &Db) -> BTreeMap<FileId, (u64, u64)> {
    let mut live: BTreeMap<FileId, (u64, u64)> = BTreeMap::new();
    for (_, link) in links_of(db) {
        let location = link.location().expect("a block");
        let (bytes, blocks) = live.entry(location.file).or_default();
        *bytes += u64::from(location.len);
        *blocks += 1;
    }
    live
}

/// The segments the store has, in the order of their ids.
pub(super) fn segment_ids(db: &Db) -> Vec<FileId> {
    db.segments
        .extents()
        .iter()
        .map(|extent| extent.file)
        .collect()
}

/// Whether the file of the segment `file` is in the directory.
pub(super) fn on_disk(db: &Db, file: FileId) -> bool {
    let path = crate::segment::FileNames::SEGMENT.path(&db.path, file);
    db.vfs.open(&path, lignin_io::OpenMode::Read).is_ok()
}

/// Checks what the accounting says against a walk over every segment and
/// a read of every chain.
pub(super) fn check_space(db: &Db, space: &Space) {
    let live = linked(db);
    let extents = db.segments.extents();
    assert_eq!(space.segments.len(), extents.len());
    for (held, extent) in space.segments.iter().zip(&extents) {
        let (bytes, blocks) = blocks_of(db, held.file);
        let (live_bytes, live_blocks) = live.get(&held.file).copied().unwrap_or_default();
        assert_eq!(held.file, extent.file);
        assert_eq!(
            held.block_bytes, bytes,
            "the blocks of segment {}",
            held.file
        );
        assert_eq!(
            (held.live_bytes, held.live_blocks),
            (live_bytes, live_blocks),
            "what is live of the {blocks} blocks of segment {}",
            held.file
        );
        assert_eq!(held.dead_bytes(), bytes - live_bytes);
        assert!(held.pinned_bytes <= held.live_bytes);
        assert_eq!(held.state, extent.state);
    }
}

/// What the segments of `db` hold, for a test that fails.
pub(super) fn describe(db: &Db, space: &Space) -> String {
    let mut out = String::new();
    for segment in &space.segments {
        out.push_str(&format!(
            "\nsegment {}: {:?}{} {} bytes, {} live in {} blocks, {} pinned{}",
            segment.file,
            segment.state,
            if segment.has_writer { " (written)" } else { "" },
            segment.block_bytes,
            segment.live_bytes,
            segment.live_blocks,
            segment.pinned_bytes,
            if segment.retired { ", retired" } else { "" },
        ));
    }
    for id in 1..=PAGES {
        out.push_str(&format!("\npage {id}: {}", db.shape(page(id))));
    }
    out
}

/// Passes of `cleaner`, until one moves nothing on. Returns what they did
/// in all.
pub(super) fn settle(cleaner: &Cleaner<'_>) -> Pass {
    let mut all = Pass::default();
    for _ in 0..64 {
        let pass = cleaner.pass().expect("a pass");
        all.links_trimmed += pass.links_trimmed;
        all.blocks_moved += pass.blocks_moved;
        all.bytes_moved += pass.bytes_moved;
        all.blocks_lost += pass.blocks_lost;
        all.retired += pass.retired;
        all.dropped += pass.dropped;
        all.waiting = pass.waiting;
        all.scanned |= pass.scanned;
        if !pass.progressed() && !pass.more {
            return all;
        }
    }
    panic!("the cleaner does not come to rest: {all:?}");
}

/// The pages of [`churned`].
pub(super) const PAGES: u32 = 6;

/// A store with a few pages in many small segments, most of whose blocks
/// newer ones have superseded: `rounds` commits, written back one by one.
/// The commit of round *r* changes a hash in every page whose id divides
/// *r*, so the pages are written at different rates, and what is live
/// lies scattered among what is dead.
pub(super) fn churned(rounds: u8) -> Db {
    let db = Db::with(small_segments());
    let cells: Vec<Op> = (1..=PAGES)
        .map(|id| insert((id, 1), b"key", inline(&noise(u64::from(id), 120))))
        .collect();
    db.commit(&cells);
    db.wb().pass().expect("a pass");
    for round in 1..=rounds {
        churn(&db, round);
    }
    db
}

/// One more round of [`churned`].
pub(super) fn churn(db: &Db, round: u8) {
    let changes: Vec<Op> = (1..=PAGES)
        .filter(|id| u32::from(round) % id == 0)
        .map(|id| hash((id, 1), round))
        .collect();
    db.commit(&changes);
    db.wb().pass().expect("a pass");
}

/// A block that a pass of an eager cleaner moves: one with a link, in a
/// segment that takes no more blocks and holds something dead. With its
/// page.
pub(super) fn movable(db: &Db, space: &Space) -> (PageId, BlockLocation) {
    let mixed = |file: FileId| {
        let held = space.segment(file).expect("the segment");
        !held.has_writer && held.dead_bytes() != 0
    };
    links_of(db)
        .into_iter()
        .filter_map(|(id, link)| Some((id, link.location()?)))
        .find(|(_, location)| mixed(location.file))
        .expect("a live block beside dead ones")
}

/// The cells of the tree pages of write-back's test store, as the owners
/// of their values: what the tree layer is to a cleaning of the value
/// log.
pub(super) struct Cells<'d>(pub(super) &'d Db);

impl ValueOwners for Cells<'_> {
    type Found = (SlotId, CommitVersion);

    fn find(&self, id: PageId, pointer: ValuePointer) -> Result<Option<Self::Found>, OwnersError> {
        let Some(frame) = self.0.pool.page_step(id).current() else {
            return Ok(None);
        };
        if is_flat(&frame) {
            return Ok(None);
        }
        let tree = TreePage::from_validated(&frame, FRAME_FORMAT)?;
        let holder = tree
            .cells()
            .find(|(_, cell)| cell.value_pointer() == Some(pointer));
        Ok(holder.map(|(slot, _)| (slot, frame.born())))
    }

    fn rewrite(
        &self,
        step: PointerRewrite<'_>,
        (slot, born): Self::Found,
    ) -> Result<Rewrite, OwnersError> {
        use lignin_format::{
            tree_page::{CellValue, TreePageMut},
            value_log::ValuePlace,
        };
        let db = self.0;
        let (id, from, to) = (step.page(), step.from(), step.to());
        let guard = db.pool.page_step(id);
        let Some(old) = guard.current() else {
            return Ok(Rewrite::Refused(Refusal::OwnerGone));
        };
        if old.born() != born || guard.private_copies() > 0 {
            return Ok(Rewrite::Refused(Refusal::Busy));
        }
        let holds = TreePage::from_validated(&old, FRAME_FORMAT)?
            .cell(slot)
            .is_some_and(|cell| cell.value_pointer() == Some(from));
        if !holds {
            return Ok(Rewrite::Refused(Refusal::OwnerGone));
        }
        let change = ValueMove::in_tree(id, slot, ValuePlace::Placed(from), to)?;
        let mut record = Vec::new();
        change.write(&mut record)?;
        db.wb().append_change(&record)?;
        let mut blank = db.pool.blank();
        blank.copy_from(&old);
        TreePageMut::open(&mut blank, db.size, FRAME_FORMAT)?
            .set_value(slot, CellValue::OutOfLine(to))?;
        guard.replace(&old, blank)?;
        Ok(Rewrite::Installed(step.install(db.latest())))
    }
}
