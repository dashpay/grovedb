//! The directory kept from the reports of fragment maintenance
//! (`tree-layout.md` S3): after every write the entry of every Merk names
//! the cell of its root, the one the pointer in its host's cell names,
//! also for the Merks whose roots a write carried without opening them.

use std::collections::BTreeMap;

use lignin_format::PageSize;

use super::*;
use crate::fragment::{tests::grove::Grove as Trees, Layout, Report};

/// A grove in tree pages, and the directory of its Merks.
struct Kept {
    trees: Trees,
    pages: Pages,
    directory: Directory,
    /// The Merks that are entered, by their number in the grove.
    entered: BTreeMap<usize, Prefix>,
    /// The keys that were put, by Merk.
    keys: Vec<(usize, Vec<u8>)>,
    next_key: u64,
    /// The roots that were carried, and how many of them were of a Merk
    /// that the write did not write.
    carried: (u64, u64),
    dropped: Vec<Dropped>,
}

fn prefix(merk: usize) -> Prefix {
    match merk {
        0 => prefix_of(&[]),
        merk => prefix_of(&[(merk.to_le_bytes().to_vec(), 0)]),
    }
}

impl Kept {
    fn new(seed: u64) -> Self {
        let (mut pages, directory) = Pages::directory();
        directory
            .add(&mut pages, &prefix(0), b"grove", None)
            .expect("entered");
        Self {
            trees: Trees::new(Layout::proposed(PageSize::MIN), seed),
            pages,
            directory,
            entered: BTreeMap::from([(0, prefix(0))]),
            keys: Vec::new(),
            next_key: 0,
            carried: (0, 0),
            dropped: Vec::new(),
        }
    }

    /// The Merk that hosts `merk`, with the host's node.
    fn above(&self, merk: usize) -> Option<(usize, usize)> {
        let host = self.trees.merks[merk].host?;
        Some((self.trees.nodes[host].merk, host))
    }

    /// The root of `merk` as the tree pages have it.
    fn root(&self, merk: usize) -> RootAt {
        let root = &self.trees.nodes[self.trees.merks[merk].root?];
        Some((root.at.expect("a root has a cell"), root.key.clone()))
    }

    /// Tells the directory what the write of `merk` did to the roots: the
    /// Merks from `merk` up to the root of the grove by their prefix, as
    /// the write wrote each of them, and every other carried root by the
    /// address it had.
    fn follow(&mut self, merk: usize, report: &Report) {
        let mut written = vec![merk];
        while let Some((above, _)) = self.above(written[written.len() - 1]) {
            written.push(above);
        }
        let roots: Vec<(Prefix, RootAt)> = written
            .iter()
            .map(|merk| (self.entered[merk], self.root(*merk)))
            .collect();
        let mut changes: Vec<RootChange<'_>> = roots
            .iter()
            .map(|(prefix, root)| named(prefix, root.as_ref().map(|(at, key)| (*at, &key[..]))))
            .collect();
        let written: Vec<Option<CellAddress>> = roots
            .iter()
            .map(|(_, root)| root.as_ref().map(|(at, _)| *at))
            .collect();
        for carried in &report.carried {
            let Some(change) = RootChange::carried(carried) else {
                continue;
            };
            self.carried.0 += 1;
            self.carried.1 += u64::from(!written.contains(&Some(carried.to.address())));
            changes.push(change);
        }
        self.directory
            .set_roots(&mut self.pages, &changes)
            .expect("the roots are stated");
    }

    fn merk(&mut self) -> usize {
        loop {
            let pick = self.trees.rng.below(self.trees.merks.len());
            if self.trees.merks[pick].alive {
                return pick;
            }
        }
    }

    fn key(&mut self) -> Option<(usize, Vec<u8>)> {
        while !self.keys.is_empty() {
            let pick = self.trees.rng.below(self.keys.len());
            let (merk, key) = &self.keys[pick];
            if self.trees.merks[*merk].alive && self.trees.find(*merk, key).is_some() {
                return Some(self.keys[pick].clone());
            }
            self.keys.swap_remove(pick);
        }
        None
    }

    fn value(&mut self) -> Vec<u8> {
        let len = match self.trees.rng.below(10) {
            0..7 => self.trees.rng.below(200),
            _ => 300 + self.trees.rng.below(900),
        };
        vec![len as u8; len]
    }

    fn step(&mut self) {
        match self.trees.rng.below(100) {
            0..52 => {
                let merk = self.merk();
                self.next_key += 1;
                let mut key = self.trees.rng.next().to_be_bytes()[..3].to_vec();
                key.extend_from_slice(&self.next_key.to_be_bytes());
                let value = self.value();
                let report = self.trees.put(merk, &key, &value);
                self.keys.push((merk, key));
                self.follow(merk, &report);
            }
            52..62 => {
                if let Some((merk, key)) = self.key() {
                    let value = self.value();
                    let report = self.trees.put(merk, &key, &value);
                    self.follow(merk, &report);
                }
            }
            62..76 => {
                let Some((merk, key)) = self.key() else {
                    return;
                };
                // The Merks that the node hosts go with it.
                let node = self.trees.find(merk, &key).expect("the key is there");
                let hosted = self.trees.nodes[node].hosted.clone();
                let report = self.trees.delete(merk, &key);
                for hosted in hosted {
                    let prefix = self.entered.remove(&hosted).expect("entered");
                    let dropped = self.directory.unlink(&mut self.pages, &prefix);
                    self.dropped.push(dropped.expect("dropped"));
                }
                self.follow(merk, &report);
            }
            76..86 => {
                let Some((merk, key)) = self.key() else {
                    return;
                };
                let node = self.trees.find(merk, &key).expect("the key is there");
                let position = self.trees.nodes[node].hosted.len() as u8;
                let (hosted, report) = self.trees.host_reported(merk, &key, false);
                let parent = self.entered[&merk];
                let host = Host {
                    parent: &parent,
                    key: &key,
                    position,
                };
                let added =
                    self.directory
                        .add(&mut self.pages, &prefix(hosted), b"tree", Some(host));
                added.expect("entered");
                self.entered.insert(hosted, prefix(hosted));
                self.follow(merk, &report);
            }
            86..89 => {
                let merk = self.merk();
                let Some((above, _)) = self.above(merk) else {
                    return;
                };
                let report = self.trees.clear(merk);
                let prefix = self.entered[&merk];
                let cleared = self
                    .directory
                    .clear(&mut self.pages, &prefix)
                    .expect("cleared");
                self.dropped.extend(cleared);
                self.follow(above, &report);
            }
            89..96 => {
                if let Some((merk, key)) = self.key() {
                    let report = self.trees.touch(merk, &key);
                    self.follow(merk, &report);
                }
            }
            _ => {
                // The reclaimer takes what one dropped Merk left in the
                // directory, before it takes the cells.
                if self.dropped.is_empty() {
                    return;
                }
                let pick = self.trees.rng.below(self.dropped.len());
                let dropped = self.dropped.swap_remove(pick).id;
                let mut above = vec![dropped];
                while let Some(&parent) = above.last() {
                    let child = self.directory.children(parent).next(&self.pages);
                    let Some(child) = child.expect("a child record") else {
                        above.pop();
                        continue;
                    };
                    let reclaimed = self
                        .directory
                        .reclaim_child(&mut self.pages, parent, &child);
                    if matches!(reclaimed, Err(DirectoryError::Nested { .. })) {
                        above.push(child.id);
                    } else {
                        reclaimed.expect("reclaimed");
                    }
                }
                self.directory
                    .reclaimed(&mut self.pages, dropped)
                    .expect("reclaimed");
            }
        }
        // A Merk that died with the Merk above it is entered no more.
        self.entered.retain(|merk, _| self.trees.merks[*merk].alive);
    }

    /// Every Merk of the grove is opened with the root the tree pages
    /// have, and with the host that points at that root.
    fn verify(&self) {
        let census = self.directory.check(&self.pages).expect("a directory");
        assert_eq!(census.live, self.entered.len() as u64);
        for (merk, state) in self.trees.merks.iter().enumerate() {
            let opened = self
                .directory
                .open(&self.pages, &prefix(merk))
                .expect("a lookup");
            if !state.alive {
                assert!(opened.is_none(), "a dropped Merk is opened by nobody");
                continue;
            }
            let entry = opened.expect("a Merk of the grove is entered");
            let root = entry.root().map(|root| (root.address, root.key.to_vec()));
            assert_eq!(root, self.root(merk), "the entry names the root's cell");
            let Some((above, host)) = self.above(merk) else {
                assert_eq!(entry.host(), None);
                continue;
            };
            // The element that hosts it, and the pointer in that element's
            // cell: the same address as the entry's.
            let node = &self.trees.nodes[host];
            let named = entry.host().expect("a host");
            assert_eq!((named.parent, named.key), (&prefix(above), &node.key[..]));
            assert_eq!(node.hosted[usize::from(named.position)], merk);
            let at = node.at.expect("the host has a cell");
            let view = self.trees.mem.view(at.page());
            let cell = view.node(at.slot()).expect("the host's cell");
            let pointer = cell
                .hosted_roots()
                .nth(usize::from(named.position))
                .flatten();
            assert_eq!(pointer, entry.root().map(|root| root.address));
        }
    }
}

#[test]
fn a_directory_kept_from_the_reports_of_the_writes_names_every_root() {
    let mut carried = (0, 0);
    let mut merks = 0;
    for seed in 1..=4 {
        let mut kept = Kept::new(seed);
        for step in 0..2_500 {
            kept.step();
            kept.verify();
            if step % 50 == 0 {
                kept.trees.check();
            }
        }
        kept.trees.check();
        carried = (carried.0 + kept.carried.0, carried.1 + kept.carried.1);
        merks += kept.trees.merks.len();
    }
    println!(
        "{merks} Merks in four groves: {} roots were carried, {} of them of Merks the write did \
         not write",
        carried.0, carried.1
    );
    assert!(
        carried.1 > 50,
        "roots of Merks that no write opened were carried"
    );
}
