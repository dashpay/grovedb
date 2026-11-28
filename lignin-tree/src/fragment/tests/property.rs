//! The acceptance test of TREE-03: any sequence of physical
//! reorganisations leaves the logical tree, its keys, values and shape,
//! unchanged.

use std::fmt::Write as _;

use lignin_format::tree_page::{Side, TreePage};

use super::{
    grove::{digest, Grove},
    *,
};

/// Drives a grove with random writes and compares the pages with it.
struct Run {
    grove: Grove,
    /// The keys that were put, by Merk. Some are gone again.
    keys: Vec<(usize, Vec<u8>)>,
    /// The Merks that were made.
    merks: Vec<usize>,
    next_key: u64,
}

impl Run {
    fn new(size: PageSize, seed: u64) -> Self {
        Self {
            grove: Grove::new(Layout::proposed(size), seed),
            keys: Vec::new(),
            merks: vec![0],
            next_key: 0,
        }
    }

    fn value(&mut self) -> Vec<u8> {
        let rng = &mut self.grove.rng;
        let len = match rng.below(100) {
            0..55 => rng.below(120),
            55..75 => 120 + rng.below(136),
            75..80 => 257 + rng.below(40),
            _ => 300 + rng.below(1500),
        };
        let seed = rng.next();
        (0..len).map(|index| (seed >> (index % 56)) as u8).collect()
    }

    fn merk(&mut self) -> usize {
        loop {
            let pick = self.grove.rng.below(self.merks.len());
            if self.grove.merks[self.merks[pick]].alive {
                return self.merks[pick];
            }
            self.merks.swap_remove(pick);
        }
    }

    /// A key that is in the grove, if any is left.
    fn key(&mut self) -> Option<(usize, Vec<u8>)> {
        while !self.keys.is_empty() {
            let pick = self.grove.rng.below(self.keys.len());
            let (merk, key) = &self.keys[pick];
            if self.grove.merks[*merk].alive && self.grove.find(*merk, key).is_some() {
                return Some(self.keys[pick].clone());
            }
            self.keys.swap_remove(pick);
        }
        None
    }

    /// How many cells the writes so far carried.
    fn carried(&self) -> u64 {
        self.grove.fragments.counters().carries.total().cells
    }

    fn insert(&mut self) {
        let merk = self.merk();
        self.next_key += 1;
        let mut key = self.grove.rng.next().to_be_bytes().to_vec();
        key.truncate(1 + self.grove.rng.below(8));
        key.extend_from_slice(&self.next_key.to_be_bytes());
        let value = self.value();
        self.grove.put(merk, &key, &value);
        self.keys.push((merk, key));
    }

    fn step(&mut self) {
        let dice = self.grove.rng.below(100);
        match dice {
            0..50 => self.insert(),
            50..62 => {
                if let Some((merk, key)) = self.key() {
                    let value = self.value();
                    self.grove.put(merk, &key, &value);
                }
            }
            62..84 => {
                if let Some((merk, key)) = self.key() {
                    self.grove.delete(merk, &key);
                }
            }
            84..88 => {
                if let Some((merk, key)) = self.key() {
                    let wide = self.grove.rng.chance(40);
                    let hosted = self.grove.host(merk, &key, wide);
                    self.merks.push(hosted);
                }
            }
            88..90 => {
                let merk = self.merk();
                if merk != 0 {
                    self.grove.clear(merk);
                }
            }
            90..96 => {
                if let Some((merk, key)) = self.key() {
                    self.grove.touch(merk, &key);
                }
            }
            _ => {
                let Grove { log, mem, .. } = &mut self.grove;
                log.write_back(mem);
            }
        }
    }

    /// Writes nodes again as they are, under `layout`. The logical tree
    /// must stay what it is, whatever the write carries.
    fn reorganise(&mut self, layout: Layout, writes: usize) -> u64 {
        self.grove.fragments.set_layout(layout);
        let before = self.grove.logical();
        let carried = self.carried();
        for _ in 0..writes {
            let Some((merk, key)) = self.key() else { break };
            let report = self.grove.touch(merk, &key);
            assert!(report.freed.len() <= report.left.len());
            assert!(
                before == self.grove.logical(),
                "the logical tree is what it was"
            );
        }
        self.grove.check();
        assert!(
            before == self.grove.logical(),
            "the logical tree is what it was"
        );
        self.carried() - carried
    }
}

fn layouts(size: PageSize) -> [Layout; 5] {
    let layout = |largest, small, seed, merge| {
        let params = LayoutParams {
            largest_fragment_percent: largest,
            small_fragment_divisor: small,
            seed_limit_percent: seed,
            merge_below_percent: merge,
        };
        Layout::new(size, params).expect("a layout")
    };
    [
        Layout::proposed(size),
        layout(100, 8, 0, 0),
        layout(25, 32, 90, 60),
        layout(50, 16, 60, 20),
        layout(100, 4, 75, 35),
    ]
}

#[test]
fn any_sequence_of_reorganisations_leaves_the_logical_tree_unchanged() {
    let mut carried = 0;
    let mut reorganised = 0;
    let mut pages = 0;
    let mut rotations = 0;
    for seed in 1..=6 {
        let size = [PageSize::MIN, PageSize::new(16_384).expect("a page size")][seed as usize % 2];
        let mut run = Run::new(size, seed);
        run.grove.restate = seed % 3 == 0;
        let layouts = layouts(size);
        for step in 0..2600 {
            run.step();
            if step % 4 == 0 {
                run.grove.check();
            }
            if step % 400 == 399 {
                let layout = layouts[run.grove.rng.below(layouts.len())];
                reorganised += run.reorganise(layout, 60);
            }
        }
        // The same nodes under each layout in turn: only cells move.
        for layout in layouts.into_iter().rev() {
            reorganised += run.reorganise(layout, 300);
        }
        run.grove.check();
        carried += run.carried();
        rotations += run.grove.rotations;
        pages += run.grove.mem.frames.len();
        let counters = run.grove.fragments.counters();
        assert_eq!(
            counters.pages_created - counters.pages_freed,
            run.grove.mem.frames.len() as u64
        );
    }
    println!(
        "{pages} pages in six groves, {rotations} rotations, {carried} cells carried, {reorganised} \
         of them by writes that changed no node"
    );
    assert!(rotations > 2000, "the trees rotated: {rotations}");
    assert!(pages > 60, "the groves span many pages: {pages}");
    assert!(carried > 3000, "the writes carried cells: {carried}");
    assert!(
        reorganised > 100,
        "writes that changed no node carried cells: {reorganised}"
    );
}

/// Writes that stop below the host of their Merk (`tree-layout.md` T3),
/// among writes that go up to the root of the grove and carry the hosts
/// that are behind. Whenever no host is behind, the pages are the grove.
#[test]
fn writes_that_stop_below_a_host_are_put_right_when_the_host_is_written() {
    let (mut stopped, mut finalized, mut checked) = (0, 0, 0);
    let (mut carried_behind, mut refused, mut renamed) = (0, 0, 0);
    for seed in 1..=4 {
        let size = [PageSize::MIN, PageSize::new(16_384).expect("a page size")][seed as usize % 2];
        let mut run = Run::new(size, 70 + seed);
        run.grove.restate = seed == 4;
        for step in 0..2400 {
            let dice = run.grove.rng.below(100);
            let merk = Some(run.merk()).filter(|merk| *merk != 0);
            let any = run.key();
            let key = any.clone().filter(|(merk, _)| *merk != 0);
            match (dice, merk, key) {
                (0..22, Some(merk), _) => {
                    run.next_key += 1;
                    let key = [&[9][..], &run.next_key.to_be_bytes()].concat();
                    let value = run.value();
                    run.grove.put_stopped(merk, &key, &value);
                    run.keys.push((merk, key));
                    stopped += 1;
                }
                (22..30, _, Some((merk, key))) => {
                    let value = run.value();
                    run.grove.put_stopped(merk, &key, &value);
                    stopped += 1;
                }
                (30..42, _, Some((merk, key))) => {
                    run.grove.delete_stopped(merk, &key);
                    stopped += 1;
                }
                (42..44, Some(merk), _) => {
                    run.grove.clear_stopped(merk);
                    stopped += 1;
                }
                (56..59, ..) => {
                    // A node moves to another key with the Merks it hosts.
                    if let Some((merk, key)) = any {
                        run.next_key += 1;
                        let new = [&[8][..], &run.next_key.to_be_bytes()].concat();
                        run.grove.rename(merk, &key, &new);
                        run.keys.push((merk, new));
                        renamed += 1;
                    }
                }
                (44..56, ..) => {
                    let behind = run.grove.behind();
                    if !behind.is_empty() {
                        let merk = behind[run.grove.rng.below(behind.len())];
                        run.grove.finalize(merk);
                        finalized += 1;
                    }
                }
                _ => run.step(),
            }
            for merk in run.grove.behind() {
                run.grove.check_merk(merk);
            }
            if step % 25 == 24 {
                while let Some(&merk) = run.grove.behind().first() {
                    run.grove.finalize(merk);
                    finalized += 1;
                }
            }
            if !run.grove.stale {
                run.grove.check();
                checked += 1;
            }
        }
        carried_behind += run.grove.carried_behind;
        refused += run.grove.refused;
    }
    println!(
        "{stopped} writes stopped below a host, {refused} of them refused for room and made \
         again with the host, {finalized} hosts written after them, {renamed} nodes moved to \
         another key, {carried_behind} cells of hosts that were behind carried, {checked} full \
         checks"
    );
    assert!(renamed > 100, "nodes were moved to other keys: {renamed}");
    assert!(stopped > 1500 && finalized > 400 && checked > 400);
    assert!(carried_behind > 20, "hosts that were behind were carried");
}

#[test]
fn a_tree_that_only_grows_fills_its_pages_as_the_layout_says() {
    let mut run = Run::new(PageSize::MIN, 11);
    for _ in 0..6000 {
        run.insert();
    }
    run.grove.check();
    let counters = *run.grove.fragments.counters();
    let fill = run.grove.fill();
    let per_insert = counters.carries.total().cells as f64 / 6000.0;
    println!(
        "{} pages, fill {:.1}%, {per_insert:.2} cells carried per insert, {} by a split, {} by a \
         merge",
        run.grove.mem.frames.len(),
        fill * 100.0,
        counters.carries[Cause::Split].cells,
        counters.carries[Cause::Merge].cells,
    );
    assert!(fill > 0.60, "fill is {fill}");
    assert!(
        per_insert < 1.6,
        "{per_insert} cells are carried for each one inserted"
    );
    assert_eq!(counters.carries[Cause::Delete].cells, 0);
    assert_eq!(counters.created, 6000);
}

/// Where the pages have every cell: each page with its cells by their
/// slots, and of each cell its key, where its links lead, the roots it
/// hosts and whether it is the root of a fragment. Returns the pages, the
/// cells, the hosted roots and the digest of that listing.
fn placement(mem: &Mem) -> (usize, usize, usize, String) {
    let (mut cells, mut hosted) = (0, 0);
    let mut listing = String::new();
    for (id, frame) in &mem.frames {
        let page = TreePage::from_validated(frame, FRAME_FORMAT).expect("a tree page");
        writeln!(listing, "page {id}").expect("a line");
        for (slot, cell) in page.cells() {
            cells += 1;
            write!(listing, " slot {slot} key {:02x?}", cell.key()).expect("a line");
            for side in [Side::Left, Side::Right] {
                match cell.link(side) {
                    Some(link) => write!(listing, " {}", link.child).expect("a line"),
                    None => listing.push_str(" -"),
                }
            }
            for root in cell.hosted_roots() {
                hosted += usize::from(root.is_some());
                match root {
                    Some(root) => write!(listing, " hosts {root}").expect("a line"),
                    None => listing.push_str(" hosts none"),
                }
            }
            if cell.is_fragment_root() {
                listing.push_str(" root");
            }
            listing.push('\n');
        }
    }
    let digest: String = (digest(&[listing.as_bytes()]).iter())
        .map(|byte| format!("{byte:02x}"))
        .collect();
    (mem.frames.len(), cells, hosted, digest)
}

/// The layout is a decision (`tree-layout.md`): seeded runs of writes
/// under the proposed layout leave every cell in the page and the slot
/// that are held here, with its links, the roots it hosts and the roots
/// of the fragments. A change of the planner that is meant to leave the
/// layout alone passes this unchanged. One that moves a cell of these
/// runs fails it, and whoever makes it decides the new layout and writes
/// its digests here.
///
/// The pages are those of the test, in memory: nothing but a run's own
/// steps changes them. The runs are of three page sizes, the size of a
/// store's pages among them, and in two of them a written node whose
/// value is kept, and waits for its place, states that value again.
#[test]
fn seeded_runs_leave_every_cell_where_the_layout_was_decided() {
    // The seed, the page size, the steps and whether a value that waits
    // is stated again; then the pages, the cells, the hosted roots and
    // the digest.
    type Held = (
        (u64, usize, usize, bool),
        (usize, usize, usize, &'static str),
    );
    const HELD: [Held; 5] = [
        (
            (20_261_124, 8192, 4000, false),
            (
                50,
                1162,
                46,
                "d6655613345762911bee31d801ba5ae40b7887f9f6d3f3ab75a938deba54e979",
            ),
        ),
        (
            (20_261_125, 16_384, 6000, false),
            (
                35,
                1743,
                83,
                "e88428d301ffd56ef51e08105892a09dc40fd6b3e8d291d808fa05abbad625ee",
            ),
        ),
        (
            (20_261_126, 32_768, 9000, false),
            (
                25,
                2523,
                119,
                "799de81d7fbbb12d10118d43b522e70201c5d7e018d9b55a38be0faa72174304",
            ),
        ),
        (
            (20_261_127, 8192, 4000, true),
            (
                46,
                1201,
                65,
                "b48948b21bdb0ab1d30f1fcd79aa03efa6e5a1e943c8c788489f6b8f1924939e",
            ),
        ),
        (
            (20_261_128, 16_384, 6000, true),
            (
                35,
                1720,
                62,
                "bdd8e7a55fe274ce47cff6a84397e3b8aa354647b805819f87a39bf00fcbb052",
            ),
        ),
    ];
    let mut found = Vec::new();
    for ((seed, size, steps, restate), _) in HELD {
        let size = PageSize::new(size).expect("a page size");
        let mut run = Run::new(size, seed);
        run.grove.restate = restate;
        for step in 0..steps {
            run.step();
            if step % 1000 == 999 {
                run.grove.check();
            }
        }
        run.grove.check();
        let (pages, cells, hosted, digest) = placement(&run.grove.mem);
        assert!(
            pages > 12 && cells > 600 && hosted > 20,
            "the run of {seed} spans pages and hosts Merks: {pages} pages, {cells} cells, \
             {hosted} hosted roots"
        );
        found.push((
            (seed, size.get(), steps, restate),
            (pages, cells, hosted, digest),
        ));
    }
    let found: Vec<_> = (found.iter())
        .map(|(run, (pages, cells, hosted, digest))| (*run, (*pages, *cells, *hosted, &digest[..])))
        .collect();
    assert_eq!(
        found, HELD,
        "the layout of the tree pages is not the one that was decided"
    );
}
