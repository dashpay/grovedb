//! What a run of the checker returns.

use std::{fmt, time::Duration};

use lignin_format::{CellAddress, FileId};
use lignin_store::alloc;
use lignin_tree::directory::Prefix;

use super::finding::{Finding, Rule};

/// The cells that nothing reaches: no root of a Merk in use, and no record
/// of a dropped Merk.
///
/// They are space and no state (`tree-layout.md` D2, section 17 item 15).
/// A reclaiming that is under way has them, because the record of a dropped
/// Merk goes before its cells, and a crash while the cells of one Merk go
/// leaves the rest of them for good. No read returns them. They are counted
/// here and are no [`Finding`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Unreached {
    /// The cells.
    pub cells: u64,
    /// The bytes they take in their pages, their slots included.
    pub bytes: u64,
    /// The tree pages that hold at least one of them.
    pub pages: u64,
    /// The tree pages that hold no cell at all.
    pub empty_pages: u64,
    /// The values out of line that they hold, which stay counted as live.
    pub values: u64,
    /// The first of them, in the order of their addresses.
    pub first: Vec<CellAddress>,
    /// Whether a walk was pointed at a page that is not read, or the
    /// directory was not read. The cells that only such a page or the
    /// directory leads to are then counted here with the others, and the
    /// cells of the page itself are not counted at all. The run has a
    /// finding that says what was not read.
    pub uncertain: bool,
}

impl Unreached {
    /// How many addresses [`Self::first`] holds at most.
    pub const SAMPLE: usize = 16;

    /// Whether the store holds space that nothing reaches.
    pub const fn is_empty(&self) -> bool {
        self.cells == 0 && self.empty_pages == 0
    }
}

/// A value hash that is not the one the rules make, and that the rules
/// name all the same ([`OtherHashes`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OtherHash {
    /// The cell that stores it.
    pub cell: CellAddress,
    /// The prefix of the Merk the cell is in, where the directory names
    /// the Merk.
    pub subtree: Option<Prefix>,
    /// The key of the cell's node.
    pub key: Vec<u8>,
}

/// The value hashes of cells in use that are not the ones the rules make,
/// and that the rules name as made by a writer with another hash of a
/// hosted root (`lignin_tree::hash::made_with_other`).
///
/// The rules say of such a hash that a writer made it so. The checker
/// takes their word: it is counted here and is no [`Finding`]. Rules that
/// name no other hash, as the
/// provided methods of [`HashRules`](lignin_tree::hash::HashRules) do,
/// leave this empty, and every value hash that differs is a finding.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OtherHashes {
    /// How many there are.
    pub count: u64,
    /// The first of them, in the order the walks came to them.
    pub first: Vec<OtherHash>,
}

impl OtherHashes {
    /// How many [`Self::first`] holds at most.
    pub const SAMPLE: usize = 16;

    /// Whether the run came to none.
    pub const fn is_empty(&self) -> bool {
        self.count == 0
    }
}

/// How much of one segment the page table names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SegmentUse {
    /// The segment.
    pub file: FileId,
    /// The blocks that a link of the page table names.
    pub blocks: u64,
    /// The bytes of those blocks, their frames included.
    pub bytes: u64,
}

/// The pointers that the pages hold into one value-log file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ValueUse {
    /// The file.
    pub file: FileId,
    /// The cells and flat records that hold a pointer into it.
    pub values: u64,
    /// The bytes those values take in the file, record headers included.
    pub bytes: u64,
}

/// What a run counted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Census {
    /// The tree pages of the version.
    pub tree_pages: u64,
    /// The flat pages of the version.
    pub flat_pages: u64,
    /// The cells of all tree pages.
    pub cells: u64,
    /// The cells of the Merks in use.
    pub live_cells: u64,
    /// The cells that dropped Merks left and that are reached from their
    /// records.
    pub left_cells: u64,
    /// The Merks in use. In a store with no directory, those that have a
    /// node: the roots that were given and every hosted root below them.
    pub merks: u64,
    /// The entries of Merks below a dropped one.
    pub left_over_entries: u64,
    /// The dropped and cleared Merks that have their record.
    pub dropped_merks: u64,
    /// The pointers to hosted roots that were followed.
    pub hosted_roots: u64,
    /// The cells whose hashes were recomputed.
    pub hashed_cells: u64,
    /// The records of the flat keyspaces, long keys included.
    pub flat_records: u64,
    /// The flat pages that range deletes left and that are not freed yet.
    pub left_flat_pages: u64,
    /// The values in the value log that a cell or a flat record holds.
    pub placed_values: u64,
    /// The values that wait for write-back to place them.
    pub pending_values: u64,
    /// The pointers into each value-log file, in the order of the files.
    pub values: Vec<ValueUse>,
    /// The page ids by their state, where the store was still and has an
    /// allocator.
    pub ids: Option<alloc::Census>,
    /// The links of the page table that were followed to their blocks,
    /// where the store was still.
    pub links: u64,
    /// What the page table names of each segment, in the order of the
    /// files, where the store was still.
    pub segments: Vec<SegmentUse>,
}

/// How long the parts of a run took.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Phases {
    /// Reading every page once: its form, its fragments, its values.
    pub pages: Duration,
    /// The directory's own check and the walk of the flat trees.
    pub flat: Duration,
    /// The walk of the Merks, with the hashes.
    pub merks: Duration,
    /// The cells that nothing reaches, the page ids and the counts.
    pub census: Duration,
    /// The chains of the page table and their blocks.
    pub segments: Duration,
}

impl Phases {
    /// The whole run.
    pub fn total(&self) -> Duration {
        self.pages + self.flat + self.merks + self.census + self.segments
    }
}

/// What a run of the checker found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Every rule that does not hold, with the place, in the order they
    /// were found, up to the limit of the run.
    pub findings: Vec<Finding>,
    /// The findings above the limit, which are not in [`Self::findings`].
    pub more_findings: u64,
    /// The cells that nothing reaches: space, and no finding.
    pub unreached: Unreached,
    /// The value hashes that the rules name as another writer's: no
    /// finding.
    pub other_hashes: OtherHashes,
    /// What the run counted.
    pub census: Census,
    /// How long it took.
    pub phases: Phases,
}

impl Report {
    /// Whether every rule holds. Cells that nothing reaches do not make a
    /// store unsound ([`Unreached`]), and neither does a value hash that
    /// the rules name as another writer's ([`OtherHashes`]).
    pub fn is_sound(&self) -> bool {
        self.findings.is_empty() && self.more_findings == 0
    }

    /// Whether a finding is of `rule`.
    pub fn has(&self, rule: Rule) -> bool {
        self.findings.iter().any(|finding| finding.rule == rule)
    }

    /// The rules of the findings, each once, in the order they were first
    /// found.
    pub fn rules(&self) -> Vec<Rule> {
        let mut rules = Vec::new();
        for finding in &self.findings {
            if !rules.contains(&finding.rule) {
                rules.push(finding.rule);
            }
        }
        rules
    }
}

/// The report as an operator reads it: the findings, then the space that
/// nothing reaches, then the value hashes that the rules name as another
/// writer's, then the counts.
impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let census = &self.census;
        match self.findings.len() as u64 + self.more_findings {
            0 => writeln!(f, "sound: every rule holds")?,
            count => writeln!(f, "NOT SOUND: {count} findings")?,
        }
        for finding in &self.findings {
            writeln!(f, "  {finding}")?;
        }
        if self.more_findings > 0 {
            writeln!(f, "  and {} more", self.more_findings)?;
        }
        let unreached = &self.unreached;
        if !unreached.is_empty() {
            writeln!(
                f,
                "space that nothing reaches: {} cells of {} bytes in {} pages, holding {} values \
                 out of line; {} pages with no cell",
                unreached.cells,
                unreached.bytes,
                unreached.pages,
                unreached.values,
                unreached.empty_pages
            )?;
            if unreached.uncertain {
                writeln!(
                    f,
                    "  not all of it is space: what was not read may lead to some of these cells"
                )?;
            }
            for address in &unreached.first {
                writeln!(f, "  cell {address}")?;
            }
        }
        let other = &self.other_hashes;
        if !other.is_empty() {
            writeln!(
                f,
                "value hashes that are not the ones the rules make, and that the rules name as \
                 made with another hash of a hosted root: {}",
                other.count
            )?;
            for hash in &other.first {
                writeln!(f, "  cell {}, key {}", hash.cell, hex(&hash.key))?;
            }
        }
        writeln!(
            f,
            "{} tree pages with {} cells: {} of {} Merks in use, {} left by {} dropped Merks",
            census.tree_pages,
            census.cells,
            census.live_cells,
            census.merks,
            census.left_cells,
            census.dropped_merks
        )?;
        writeln!(
            f,
            "{} flat pages with {} records, {} more pages left by range deletes; {} entries \
             below dropped Merks",
            census.flat_pages,
            census.flat_records,
            census.left_flat_pages,
            census.left_over_entries
        )?;
        writeln!(
            f,
            "{} hosted roots followed, the hashes of {} cells recomputed",
            census.hosted_roots, census.hashed_cells
        )?;
        writeln!(
            f,
            "{} values in {} value-log files, {} pending",
            census.placed_values,
            census.values.len(),
            census.pending_values
        )?;
        if let Some(ids) = &census.ids {
            writeln!(
                f,
                "page ids: {} live, {} retired, {} private, {} free below the highest used",
                ids.live,
                ids.retired,
                ids.private,
                ids.free_below_high_water()
            )?;
        }
        if census.links > 0 {
            let (blocks, bytes) = (census.segments.iter()).fold((0, 0), |(blocks, bytes), used| {
                (blocks + used.blocks, bytes + used.bytes)
            });
            writeln!(
                f,
                "{} links of the page table name {blocks} blocks of {bytes} bytes in {} segments",
                census.links,
                census.segments.len()
            )?;
        }
        let phases = &self.phases;
        write!(
            f,
            "checked in {:.2} s: pages {:.2}, flat trees {:.2}, Merks {:.2}, census {:.2}, \
             segments {:.2}",
            phases.total().as_secs_f64(),
            phases.pages.as_secs_f64(),
            phases.flat.as_secs_f64(),
            phases.merks.as_secs_f64(),
            phases.census.as_secs_f64(),
            phases.segments.as_secs_f64()
        )
    }
}

/// `bytes` in hexadecimal.
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
