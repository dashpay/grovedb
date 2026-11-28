//! What a simulated layout is measured by.

use std::fmt;

use super::model::{Config, Model};

/// Why a cell was carried from one page to another.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cause {
    /// A page was full: a node went up to its parent's page, or a fragment
    /// went to a fresh page.
    Split = 0,
    /// A small fragment went to its parent's page.
    Merge = 1,
    /// A rotation lifted a node over a node of another page.
    Rotation = 2,
    /// A node took the place of a deleted node of another page.
    Delete = 3,
}

impl Cause {
    /// Every cause, in the order of [`Stats::moves`].
    pub const ALL: [Cause; 4] = [Cause::Split, Cause::Merge, Cause::Rotation, Cause::Delete];

    /// A short name for reports.
    pub fn name(self) -> &'static str {
        match self {
            Cause::Split => "split",
            Cause::Merge => "merge",
            Cause::Rotation => "rotation",
            Cause::Delete => "delete",
        }
    }
}

/// Cells carried from one page to another: the cells a transition left in
/// another page than it found them in. A cell the transition created is not
/// among them, and a cell carried twice by one transition is there once.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Moves {
    /// How many cells.
    pub cells: u64,
    /// The bytes of those cells.
    pub cell_bytes: u64,
    /// How many of them have their value out of line and did not get a new
    /// value in the same transition. Each such value is appended to the
    /// value log again (value-log-format.md, O5).
    pub values: u64,
    /// The bytes of those appends, record headers included.
    pub value_bytes: u64,
}

impl Moves {
    fn plus(self, other: Moves) -> Moves {
        Moves {
            cells: self.cells + other.cells,
            cell_bytes: self.cell_bytes + other.cell_bytes,
            values: self.values + other.values,
            value_bytes: self.value_bytes + other.value_bytes,
        }
    }

    fn minus(self, other: Moves) -> Moves {
        Moves {
            cells: self.cells - other.cells,
            cell_bytes: self.cell_bytes - other.cell_bytes,
            values: self.values - other.values,
            value_bytes: self.value_bytes - other.value_bytes,
        }
    }
}

/// Counters a [`Model`] keeps, all totals since it started.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stats {
    /// State transitions applied.
    pub transitions: u64,
    /// Blocks ended.
    pub blocks: u64,
    /// Tree pages written, summed over transitions: a page counts once for
    /// each transition that changes a cell, a link or a hash in it.
    pub dirty_pages: u64,
    /// Tree pages written, summed over blocks: a page counts once for each
    /// block that changes it.
    pub dirty_pages_by_block: u64,
    /// Subtree directory entries written, summed over transitions.
    pub directory_writes: u64,
    /// How many transitions wrote 0, 1, 2 ... pages, directory entries
    /// included. The last entry also counts everything above it.
    pub dirty_histogram: [u64; 64],
    /// The most pages one transition wrote.
    pub max_dirty: u64,
    /// Node records Merk wrote.
    pub records: u64,
    /// The bytes of those records as RocksDB is given them.
    pub record_bytes: u64,
    /// The records in which only hashes changed.
    pub hash_only_records: u64,
    /// The records whose value bytes changed.
    pub value_records: u64,
    /// Nodes created.
    pub created: u64,
    /// Nodes Merk deleted.
    pub removed: u64,
    /// Nodes that went with a dropped or cleared Merk.
    pub dropped: u64,
    /// Cells carried from one page to another, by [`Cause`].
    pub moves: [Moves; 4],
    /// The bytes of the values written that are too long for their cell,
    /// record headers included: what the value log receives without any
    /// move.
    pub out_of_line_bytes: u64,
    /// Fragments split because their page was full.
    pub splits: u64,
    /// Nodes that went up to their parent's page in a split.
    pub promoted: u64,
    /// Fragments a split sent to another page.
    pub split_off: u64,
    /// Fragments split where they were because they passed the largest
    /// size.
    pub capped: u64,
    /// Fragments that left a page for one that already held cells.
    pub packed: u64,
    /// Small fragments that left an overfull page for their parent's page.
    pub crowded_up: u64,
    /// Fragments that left an overfull page whole, for a page beside it
    /// (also counted in `packed`) or for a fresh page.
    pub crowded_out: u64,
    /// Small fragments that moved into their parent's page because it had
    /// room.
    pub absorbed: u64,
    /// Fragments that left a page a delete had nearly emptied for a page
    /// beside it.
    pub merged: u64,
    /// Nodes that moved to the page of the node whose place they took.
    pub inherited: u64,
    /// Nodes that moved to the page that held both their parent and a
    /// child.
    pub sandwiched: u64,
    /// Pages taken into use.
    pub pages_allocated: u64,
    /// Pages emptied and given back.
    pub pages_freed: u64,
    /// Cells of dropped Merks that the reclaimer removed, after the block
    /// that dropped them.
    pub reclaimed: u64,
    /// Pages the reclaimer wrote to do so: the ones that kept other cells.
    /// They are not among the pages a transition dirtied.
    pub reclaim_writes: u64,
    /// Times a page stayed above its capacity because the write held no
    /// cell in it. Any value but zero means the rules are incomplete.
    pub overfull_left: u64,
}

impl Default for Stats {
    fn default() -> Self {
        Stats {
            transitions: 0,
            blocks: 0,
            dirty_pages: 0,
            dirty_pages_by_block: 0,
            directory_writes: 0,
            dirty_histogram: [0; 64],
            max_dirty: 0,
            records: 0,
            record_bytes: 0,
            hash_only_records: 0,
            value_records: 0,
            created: 0,
            removed: 0,
            dropped: 0,
            moves: [Moves::default(); 4],
            out_of_line_bytes: 0,
            splits: 0,
            promoted: 0,
            split_off: 0,
            capped: 0,
            packed: 0,
            crowded_up: 0,
            crowded_out: 0,
            absorbed: 0,
            merged: 0,
            inherited: 0,
            sandwiched: 0,
            pages_allocated: 0,
            pages_freed: 0,
            reclaimed: 0,
            reclaim_writes: 0,
            overfull_left: 0,
        }
    }
}

impl Stats {
    /// Pages written per transition: tree pages and directory entries.
    pub fn dirty_per_transition(&self) -> f64 {
        ratio(self.dirty_pages + self.directory_writes, self.transitions)
    }

    /// Tree pages written per transition when a page counts once per block.
    pub fn dirty_per_transition_by_block(&self) -> f64 {
        ratio(self.dirty_pages_by_block, self.transitions)
    }

    /// The number of pages that `percent` of the transitions stayed at or
    /// below.
    pub fn dirty_percentile(&self, percent: u32) -> u64 {
        let wanted = (self.transitions * u64::from(percent)).div_ceil(100);
        let mut seen = 0;
        for (pages, transitions) in self.dirty_histogram.iter().enumerate() {
            seen += transitions;
            if seen >= wanted {
                return pages as u64;
            }
        }
        self.max_dirty
    }

    /// The cells carried for any cause.
    pub fn moved(&self) -> Moves {
        self.moves
            .iter()
            .fold(Moves::default(), |sum, moves| sum.plus(*moves))
    }

    /// What happened between an earlier state of the same counters and this
    /// one.
    pub fn since(&self, earlier: &Stats) -> Stats {
        let mut dirty_histogram = self.dirty_histogram;
        for (now, then) in dirty_histogram.iter_mut().zip(earlier.dirty_histogram) {
            *now -= then;
        }
        // The histogram's last entry stands for everything above it, so the
        // most pages of the interval is only known below that entry.
        let last = dirty_histogram.len() - 1;
        let max_dirty = match dirty_histogram.iter().rposition(|count| *count > 0) {
            Some(pages) if pages < last => pages as u64,
            Some(_) => self.max_dirty,
            None => 0,
        };
        let mut moves = self.moves;
        for (now, then) in moves.iter_mut().zip(earlier.moves) {
            *now = now.minus(then);
        }
        Stats {
            transitions: self.transitions - earlier.transitions,
            blocks: self.blocks - earlier.blocks,
            dirty_pages: self.dirty_pages - earlier.dirty_pages,
            dirty_pages_by_block: self.dirty_pages_by_block - earlier.dirty_pages_by_block,
            directory_writes: self.directory_writes - earlier.directory_writes,
            dirty_histogram,
            max_dirty,
            records: self.records - earlier.records,
            record_bytes: self.record_bytes - earlier.record_bytes,
            hash_only_records: self.hash_only_records - earlier.hash_only_records,
            value_records: self.value_records - earlier.value_records,
            created: self.created - earlier.created,
            removed: self.removed - earlier.removed,
            dropped: self.dropped - earlier.dropped,
            moves,
            out_of_line_bytes: self.out_of_line_bytes - earlier.out_of_line_bytes,
            splits: self.splits - earlier.splits,
            promoted: self.promoted - earlier.promoted,
            split_off: self.split_off - earlier.split_off,
            capped: self.capped - earlier.capped,
            packed: self.packed - earlier.packed,
            crowded_up: self.crowded_up - earlier.crowded_up,
            crowded_out: self.crowded_out - earlier.crowded_out,
            absorbed: self.absorbed - earlier.absorbed,
            merged: self.merged - earlier.merged,
            inherited: self.inherited - earlier.inherited,
            sandwiched: self.sandwiched - earlier.sandwiched,
            pages_allocated: self.pages_allocated - earlier.pages_allocated,
            pages_freed: self.pages_freed - earlier.pages_freed,
            reclaimed: self.reclaimed - earlier.reclaimed,
            reclaim_writes: self.reclaim_writes - earlier.reclaim_writes,
            overfull_left: self.overfull_left - earlier.overfull_left,
        }
    }
}

/// Where a layout stood at one moment.
#[derive(Clone, Debug, PartialEq)]
pub struct Sample {
    /// The counters at that moment.
    pub stats: Stats,
    /// Cells in pages: one for each node.
    pub cells: u64,
    /// Pages in use.
    pub pages: u64,
    /// The bytes of all cells.
    pub used_bytes: u64,
    /// The share of page bytes in use.
    pub fill: f64,
}

impl Sample {
    /// Nodes per page.
    pub fn nodes_per_page(&self) -> f64 {
        ratio(self.cells, self.pages)
    }
}

/// The result of one simulated layout over one workload.
#[derive(Clone, Debug, PartialEq)]
pub struct Report {
    /// The workload's name.
    pub workload: String,
    /// The parameters of the layout.
    pub config: Config,
    /// The layout when the workload ended.
    pub end: Sample,
    /// The layout at regular intervals, the end included.
    pub samples: Vec<Sample>,
    /// How many pages are 0 to 10% full, 10 to 20% and so on.
    pub fill_histogram: [u64; 10],
}

impl Report {
    /// The report of `model` after a workload.
    pub fn new(workload: &str, model: &Model) -> Self {
        let end = model.snapshot();
        let mut samples = model.samples().to_vec();
        if samples.last() != Some(&end) {
            samples.push(end.clone());
        }
        Report {
            workload: workload.to_string(),
            config: *model.config(),
            end,
            samples,
            fill_histogram: model.fill_histogram(),
        }
    }

    /// Nodes per page when the workload ended.
    pub fn nodes_per_page(&self) -> f64 {
        self.end.nodes_per_page()
    }

    /// The share of page bytes in use when the workload ended.
    pub fn fill(&self) -> f64 {
        self.end.fill
    }

    /// Pages written per transition over the whole workload.
    pub fn dirty_per_transition(&self) -> f64 {
        self.end.stats.dirty_per_transition()
    }

    /// The counters of the last tenth of the workload, when the trees are
    /// largest: what happened since the latest sample that has at most nine
    /// tenths of the transitions. Without such a sample, the whole workload.
    pub fn last_tenth(&self) -> Stats {
        let transitions = self.end.stats.transitions;
        self.samples
            .iter()
            .rev()
            .find(|sample| sample.stats.transitions * 10 <= transitions * 9)
            .map_or_else(
                || self.end.stats.clone(),
                |sample| self.end.stats.since(&sample.stats),
            )
    }

    /// The lowest fill any sample saw.
    pub fn lowest_fill(&self) -> f64 {
        self.samples
            .iter()
            .map(|sample| sample.fill)
            .fold(self.end.fill, f64::min)
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let stats = &self.end.stats;
        let last = self.last_tenth();
        let transitions = stats.transitions;
        writeln!(
            f,
            "{}: page {} B, inline up to {} B, fragments {} to {} B, seed limit {}%, merge below \
             {}%, colocate {}, carry on rotation {}, on delete {}, upkeep of {}",
            self.workload,
            self.config.page_size,
            self.config.inline_max,
            self.config.small_fragment(),
            self.config.largest_fragment(),
            self.config.seed_limit_percent,
            self.config.merge_below_percent,
            self.config.colocate,
            self.config.carry_on_rotation,
            self.config.carry_on_delete,
            match self.config.upkeep_of_what_changed {
                true => "what a write changed",
                false => "every node of a write",
            },
        )?;
        writeln!(
            f,
            "  end state      {} nodes in {} pages: {:.1} nodes per page, fill {:.1}% (lowest sample \
             {:.1}%)",
            self.end.cells,
            self.end.pages,
            self.nodes_per_page(),
            self.fill() * 100.0,
            self.lowest_fill() * 100.0,
        )?;
        writeln!(
            f,
            "  pages dirtied  {:.2} per transition ({:.2} tree pages + {:.2} directory entries); \
             last tenth {:.2}; p50 {} p99 {} max {}; counted once per block {:.2}",
            stats.dirty_per_transition(),
            ratio(stats.dirty_pages, transitions),
            ratio(stats.directory_writes, transitions),
            last.dirty_per_transition(),
            stats.dirty_percentile(50),
            stats.dirty_percentile(99),
            stats.max_dirty,
            stats.dirty_per_transition_by_block(),
        )?;
        writeln!(
            f,
            "  Merk records   {:.1} per transition, {:.0} B as RocksDB gets them; {:.1}% hash only, \
             {:.1}% with a new value",
            ratio(stats.records, transitions),
            ratio(stats.record_bytes, transitions),
            ratio(stats.hash_only_records * 100, stats.records),
            ratio(stats.value_records * 100, stats.records),
        )?;
        let moved = stats.moved();
        writeln!(
            f,
            "  cells carried  {:.3} per transition ({:.0} B of cells); values appended again \
             {:.3} per transition ({:.0} B, {:.1}% on top of the {:.0} B of out-of-line values \
             written)",
            ratio(moved.cells, transitions),
            ratio(moved.cell_bytes, transitions),
            ratio(moved.values, transitions),
            ratio(moved.value_bytes, transitions),
            ratio(moved.value_bytes * 100, stats.out_of_line_bytes),
            ratio(stats.out_of_line_bytes, transitions),
        )?;
        for cause in Cause::ALL {
            let moves = stats.moves[cause as usize];
            writeln!(
                f,
                "    by {:<9} {:.4} cells per transition, {:.4} values, {:.1} value B",
                cause.name(),
                ratio(moves.cells, transitions),
                ratio(moves.values, transitions),
                ratio(moves.value_bytes, transitions),
            )?;
        }
        writeln!(
            f,
            "  maintenance    {} splits and {} capped ({} promoted, {} split off, {} packed), {} \
             absorbed, {} merged, {} crowded up, {} crowded out, {} inherited, {} sandwiched, {} \
             left overfull",
            stats.splits,
            stats.capped,
            stats.promoted,
            stats.split_off,
            stats.packed,
            stats.absorbed,
            stats.merged,
            stats.crowded_up,
            stats.crowded_out,
            stats.inherited,
            stats.sandwiched,
            stats.overfull_left,
        )?;
        writeln!(
            f,
            "  reclaimed      {} cells of dropped Merks after their blocks, in {} page writes",
            stats.reclaimed, stats.reclaim_writes,
        )?;
        write!(f, "  fill by tenth ")?;
        for pages in self.fill_histogram {
            write!(f, " {pages}")?;
        }
        writeln!(f)
    }
}

fn ratio(numerator: u64, denominator: u64) -> f64 {
    if denominator == 0 {
        0.0
    } else {
        numerator as f64 / denominator as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stats(transitions: u64, dirty: &[u64]) -> Stats {
        let mut stats = Stats {
            transitions,
            ..Stats::default()
        };
        for pages in dirty {
            stats.dirty_histogram[*pages as usize] += 1;
            stats.dirty_pages += pages;
            stats.max_dirty = stats.max_dirty.max(*pages);
        }
        stats
    }

    #[test]
    fn percentiles_come_from_the_histogram() {
        let stats = stats(10, &[1, 1, 1, 1, 1, 2, 2, 2, 3, 9]);
        assert_eq!(stats.dirty_percentile(50), 1);
        assert_eq!(stats.dirty_percentile(80), 2);
        assert_eq!(stats.dirty_percentile(90), 3);
        assert_eq!(stats.dirty_percentile(100), 9);
        assert_eq!(stats.dirty_per_transition(), 2.3);
        assert_eq!(Stats::default().dirty_per_transition(), 0.0);
        // A transition above the histogram is counted in its last entry.
        let mut wide = Stats {
            transitions: 1,
            max_dirty: 500,
            ..Stats::default()
        };
        wide.dirty_histogram[63] = 1;
        assert_eq!(wide.dirty_percentile(100), 63);
        wide.dirty_histogram[63] = 0;
        assert_eq!(wide.dirty_percentile(100), 500);
    }

    #[test]
    fn the_difference_of_two_states_is_what_happened_between_them() {
        let mut earlier = stats(4, &[1, 1, 2, 2]);
        earlier.moves[Cause::Split as usize] = Moves {
            cells: 3,
            cell_bytes: 300,
            values: 1,
            value_bytes: 500,
        };
        let mut later = stats(6, &[1, 1, 2, 2, 5, 5]);
        later.moves[Cause::Split as usize] = Moves {
            cells: 5,
            cell_bytes: 520,
            values: 2,
            value_bytes: 1_100,
        };
        later.moves[Cause::Merge as usize].cells = 4;
        later.blocks = 2;
        later.directory_writes = 3;
        let between = later.since(&earlier);
        assert_eq!(between.transitions, 2);
        assert_eq!(between.dirty_pages, 10);
        assert_eq!(between.dirty_histogram[5], 2);
        assert_eq!(between.dirty_histogram[1], 0);
        assert_eq!(
            between.moves[Cause::Split as usize],
            Moves {
                cells: 2,
                cell_bytes: 220,
                values: 1,
                value_bytes: 600,
            }
        );
        assert_eq!(between.moved().cells, 6);
        assert_eq!(between.dirty_per_transition(), 6.5);
        assert_eq!(between.max_dirty, 5);
        assert_eq!(later.since(&later), Stats::default());
        let mut wide = stats(1, &[]);
        wide.dirty_histogram[63] = 1;
        wide.max_dirty = 500;
        assert_eq!(wide.since(&Stats::default()).max_dirty, 500);
    }

    #[test]
    fn causes_have_names() {
        let names: Vec<&str> = Cause::ALL.iter().map(|cause| cause.name()).collect();
        assert_eq!(names, ["split", "merge", "rotation", "delete"]);
        for (index, cause) in Cause::ALL.into_iter().enumerate() {
            assert_eq!(cause as usize, index);
        }
    }

    #[test]
    fn a_report_prints_every_figure() {
        let mut model = Model::new(Config::proposed());
        model.sample();
        let empty = Report::new("empty", &model);
        assert_eq!((empty.fill(), empty.nodes_per_page()), (0.0, 0.0));
        assert_eq!(empty.samples.len(), 1, "the end is not sampled twice");
        assert_eq!(empty.last_tenth(), empty.end.stats);

        let mut report = empty.clone();
        report.workload = "documents".to_string();
        report.end.cells = 1_000;
        report.end.pages = 10;
        report.end.fill = 0.75;
        // Ninety transitions of six pages, then ten of eight.
        let dirty: Vec<u64> = (0..100)
            .map(|index| if index < 90 { 6 } else { 8 })
            .collect();
        report.end.stats = stats(100, &dirty);
        report.end.stats.records = 4_600;
        report.end.stats.hash_only_records = 4_000;
        report.end.stats.out_of_line_bytes = 45_900;
        report.end.stats.moves[Cause::Split as usize].value_bytes = 30_000;
        report.samples.push(Sample {
            fill: 0.6,
            stats: stats(90, &dirty[..90]),
            ..report.end.clone()
        });
        report.samples.push(Sample {
            stats: stats(95, &dirty[..95]),
            ..report.end.clone()
        });
        report.samples.push(report.end.clone());
        assert_eq!(report.lowest_fill(), 0.0, "the first sample was empty");
        let last = report.last_tenth();
        assert_eq!(
            (last.transitions, last.dirty_pages, last.max_dirty),
            (10, 80, 8),
            "since the sample at nine tenths, not the one after it",
        );
        assert_eq!(report.nodes_per_page(), 100.0);
        assert_eq!(report.dirty_per_transition(), 6.2);
        let text = report.to_string();
        for expected in [
            "documents: page 32768 B, inline up to 256 B",
            "1000 nodes in 10 pages: 100.0 nodes per page, fill 75.0%",
            "pages dirtied  6.20 per transition",
            "last tenth 8.00",
            "p50 6 p99 8 max 8",
            "Merk records   46.0 per transition",
            "87.0% hash only",
            "65.4% on top of the 459 B",
            "by rotation  0.0000 cells per transition",
            "left overfull",
            "fill by tenth",
        ] {
            assert!(
                text.contains(expected),
                "{expected:?} is missing from\n{text}"
            );
        }
    }
}
