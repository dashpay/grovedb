//! Liveness accounting: what each segment holds that a link of the page
//! table points to, and what it holds beside that.

use std::collections::BTreeMap;

use lignin_format::{manifest::FileState, CommitVersion, FileId};

use crate::table::{Link, LinkKind};

/// What one segment holds, as one read of the page table found it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentSpace {
    /// The segment.
    pub file: FileId,
    /// How a manifest written when the read began would have named the
    /// segment.
    pub state: FileState,
    /// Whether a writer of this session had the segment when the read
    /// began. A segment that is named open, or that has a writer, is not
    /// cleaned: blocks are still appended to it.
    pub has_writer: bool,
    /// The bytes of its blocks: from the end of its file header to where
    /// its blocks end. The footer of a sealed segment is not counted.
    pub block_bytes: u64,
    /// The bytes of the blocks that a link of the page table points to,
    /// frames included.
    pub live_bytes: u64,
    /// How many such blocks there are.
    pub live_blocks: u64,
    /// Those of the live bytes that a version pin alone keeps: their links
    /// would be trimmed if the oldest pinned version were the latest one.
    pub pinned_bytes: u64,
    /// Whether the page table points into the segment no more and it only
    /// waits to be deleted: for every operation that was running when that
    /// was found to end, for a table checkpoint that does not point into
    /// it, or for a checkpoint that is being created to let go of the
    /// files.
    pub retired: bool,
}

impl SegmentSpace {
    /// The bytes of the blocks that no link points to. A reader is given
    /// none of them at any version that can be asked for.
    pub fn dead_bytes(&self) -> u64 {
        self.block_bytes.saturating_sub(self.live_bytes)
    }

    /// Whether blocks can still be appended to the segment.
    pub fn is_written(&self) -> bool {
        self.has_writer || self.state == FileState::Open
    }
}

/// What the segments of a store hold: the result of the cleaner's
/// accounting ([`Cleaner::space`](super::Cleaner::space)).
///
/// **A block is live while a link of the page table points to it**, and
/// dead otherwise (`transactions.md` CL1). The table keeps the link of a
/// block for as long as a reader at the oldest pinned version or later can
/// be given the block, so a version pin keeps blocks live, and
/// [`Self::pinned_bytes`] says how many.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Space {
    /// The segments, in the order of their ids.
    pub segments: Vec<SegmentSpace>,
    /// The oldest pinned version the read went by.
    pub oldest_pin: Option<CommitVersion>,
}

impl Space {
    fn sum(&self, of: impl Fn(&SegmentSpace) -> u64) -> u64 {
        self.segments.iter().map(of).sum()
    }

    /// The bytes of all blocks of all segments.
    pub fn block_bytes(&self) -> u64 {
        self.sum(|segment| segment.block_bytes)
    }

    /// The bytes of the blocks a link points to.
    pub fn live_bytes(&self) -> u64 {
        self.sum(|segment| segment.live_bytes)
    }

    /// The bytes of the blocks no link points to.
    pub fn dead_bytes(&self) -> u64 {
        self.sum(SegmentSpace::dead_bytes)
    }

    /// The live bytes that a version pin alone keeps
    /// (`transactions.md` PN7).
    pub fn pinned_bytes(&self) -> u64 {
        self.sum(|segment| segment.pinned_bytes)
    }

    /// The bytes of the blocks of the segments that only wait to be
    /// deleted.
    pub fn retired_bytes(&self) -> u64 {
        self.sum(|segment| match segment.retired {
            true => segment.block_bytes,
            false => 0,
        })
    }

    /// The dead bytes in a hundred of all block bytes: the dead space of
    /// the store's segments. 0 for a store with no block.
    pub fn dead_percent(&self) -> f64 {
        percent(self.dead_bytes(), self.block_bytes())
    }

    /// What the segment `file` holds.
    pub fn segment(&self, file: FileId) -> Option<&SegmentSpace> {
        let at = self
            .segments
            .binary_search_by_key(&file, |segment| segment.file);
        at.ok().map(|at| &self.segments[at])
    }
}

/// What the value log holds
/// ([`Cleaner::value_space`](super::Cleaner::value_space)).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ValueSpace {
    /// The value-log files in the store.
    pub files: u64,
    /// The bytes of their values: from the end of each file header to the
    /// end of the file as a manifest written now would name it.
    pub bytes: u64,
    /// The bytes of the values that a cell of the latest version points
    /// to.
    pub live_bytes: u64,
}

impl ValueSpace {
    /// The bytes of the values that no cell of the latest version points
    /// to.
    pub fn dead_bytes(&self) -> u64 {
        self.bytes.saturating_sub(self.live_bytes)
    }

    /// The dead bytes in a hundred of all bytes of values.
    pub fn dead_percent(&self) -> f64 {
        percent(self.dead_bytes(), self.bytes)
    }
}

fn percent(part: u64, whole: u64) -> f64 {
    match whole {
        0 => 0.0,
        whole => part as f64 * 100.0 / whole as f64,
    }
}

/// Whether `part` is more than `percent` in a hundred of `whole`.
pub(super) fn is_over(part: u64, whole: u64, percent: u8) -> bool {
    u128::from(part) * 100 > u128::from(whole) * u128::from(percent)
}

/// Whether `part` is at least `percent` in a hundred of `whole`, and not
/// nothing.
pub(super) fn is_at_least(part: u64, whole: u64, percent: u8) -> bool {
    part != 0 && u128::from(part) * 100 >= u128::from(whole) * u128::from(percent)
}

/// How many links of `chain`, from the oldest, a trim at `oldest` removes
/// (`page-table.md` T11): the links before the newest base image at or
/// below `oldest`, or, if a free link is newer than that image and at or
/// below `oldest`, that free link and every link before it.
pub(super) fn trimmable(chain: &[Link], oldest: CommitVersion) -> usize {
    let found = chain
        .iter()
        .rposition(|link| link.version <= oldest && !matches!(link.kind, LinkKind::Delta(_)));
    match found {
        Some(at) if chain[at].kind == LinkKind::Free => at + 1,
        Some(at) => at,
        None => 0,
    }
}

/// The blocks that links point to, by segment, as a read of the page table
/// adds them up.
#[derive(Debug, Default)]
pub(super) struct Live {
    segments: BTreeMap<FileId, (u64, u64, u64)>,
}

impl Live {
    /// Counts the links of `chain`, the whole chain of one page, oldest
    /// first. `oldest` and `latest` are the oldest pinned version and the
    /// latest one.
    pub(super) fn chain(&mut self, chain: &[Link], oldest: CommitVersion, latest: CommitVersion) {
        let kept_by_pin = trimmable(chain, oldest)..trimmable(chain, latest.max(oldest));
        for (at, link) in chain.iter().enumerate() {
            let Some(location) = link.location() else {
                continue;
            };
            let (bytes, blocks, pinned) = self.segments.entry(location.file).or_default();
            *bytes += u64::from(location.len);
            *blocks += 1;
            if kept_by_pin.contains(&at) {
                *pinned += u64::from(location.len);
            }
        }
    }

    /// The live bytes, the live blocks and the bytes a pin alone keeps of
    /// the segment `file`.
    pub(super) fn of(&self, file: FileId) -> (u64, u64, u64) {
        self.segments.get(&file).copied().unwrap_or_default()
    }
}
