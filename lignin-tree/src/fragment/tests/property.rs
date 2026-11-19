//! The acceptance test of TREE-03: any sequence of physical
//! reorganisations leaves the logical tree, its keys, values and shape,
//! unchanged.

use super::{grove::Grove, *};

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

fn layouts(size: PageSize) -> [Layout; 4] {
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
