//! The second acceptance sentence of TREE-05, against a model: after any
//! sequence of writes, drops, moves and steps of the reclaimer, every Merk
//! in use is opened with what the model has of it, the only other entries
//! are those below a dropped Merk that the reclaimer has not reached, and
//! no open returns one of them.

use std::collections::{BTreeMap, BTreeSet};

use lignin_io::sim::SimRng;

use super::{super::Step, *};

/// A Merk as the model has it: one for each id.
#[derive(Debug, Clone)]
struct Merk {
    /// The steps from a Merk with no host down to it.
    path: Vec<Seg>,
    prefix: Prefix,
    id: MerkId,
    root: RootAt,
    tree_type: Vec<u8>,
    /// The Merk its host is in.
    parent: Option<usize>,
    /// The Merks its nodes host, or hosted when it was dropped, by the
    /// position and the key of the host.
    children: BTreeMap<(u8, Vec<u8>), usize>,
    /// Whether it is in use.
    live: bool,
    /// Whether its entry is stored: it is in use, or was left and neither
    /// reclaimed nor replaced.
    entry: bool,
    /// Whether the record of its host is stored.
    hosted: bool,
}

#[derive(Debug, Default)]
struct Counts {
    entered: u64,
    entered_again: u64,
    roots: u64,
    carried: u64,
    swapped: u64,
    emptied: u64,
    dropped: u64,
    left: u64,
    cleared: u64,
    moved: u64,
    moved_merks: u64,
    replaced: u64,
    reclaimed_merks: u64,
    reclaimed_drops: u64,
    refused: u64,
    most_left: u64,
    both_ways: u64,
    became_roots: u64,
    took_over: u64,
}

struct Model {
    pages: Pages,
    directory: Directory,
    rng: SimRng,
    merks: Vec<Merk>,
    by_id: BTreeMap<MerkId, usize>,
    /// The Merks in use, by prefix.
    live: BTreeMap<Prefix, usize>,
    /// The dropped Merks that have their record.
    drops: Vec<usize>,
    /// Every prefix a Merk was ever entered under.
    seen: BTreeSet<Prefix>,
    next_cell: u32,
    /// The cells that a root left, or that the reclaimer took out.
    free_cells: Vec<CellAddress>,
    counts: Counts,
}

const KEYS: [&[u8]; 6] = [b"a", b"b", b"c", b"docs", b"", &[0x4C; 180]];

impl Model {
    fn new(seed: u64) -> Self {
        let (pages, directory) = Pages::directory();
        let mut model = Self {
            pages,
            directory,
            rng: SimRng::new(seed),
            merks: Vec::new(),
            by_id: BTreeMap::new(),
            live: BTreeMap::new(),
            drops: Vec::new(),
            seen: BTreeSet::new(),
            next_cell: 0,
            free_cells: Vec::new(),
            counts: Counts::default(),
        };
        model.enter(None, Vec::new(), prefix_of(&[]));
        model
    }

    fn below(&mut self, bound: usize) -> usize {
        self.rng.below(bound as u64) as usize
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.rng.below(100) < percent
    }

    fn bytes(&mut self, len: usize) -> Vec<u8> {
        (0..len).map(|_| self.rng.next_u64() as u8).collect()
    }

    /// A cell that no root is in: one that was given up, or a new one.
    fn cell(&mut self) -> CellAddress {
        if !self.free_cells.is_empty() && self.chance(60) {
            let pick = self.below(self.free_cells.len());
            return self.free_cells.swap_remove(pick);
        }
        self.next_cell += 1;
        cell(1 + self.next_cell / 40, 1 + (self.next_cell % 40) as u16)
    }

    /// A Merk in use, if `fits` one.
    fn pick(&mut self, fits: impl Fn(&Merk) -> bool) -> Option<usize> {
        let fitting: Vec<usize> = self
            .live
            .values()
            .copied()
            .filter(|index| fits(&self.merks[*index]))
            .collect();
        (!fitting.is_empty()).then(|| fitting[self.below(fitting.len())])
    }

    /// The Merks that are below `index`, or were when it was dropped, with
    /// `index` first.
    fn subtree(&self, index: usize) -> Vec<usize> {
        let mut all = vec![index];
        let mut next = 0;
        while let Some(&at) = all.get(next) {
            all.extend(self.merks[at].children.values());
            next += 1;
        }
        all
    }

    /// The entry that a dropped Merk left under `prefix` is another
    /// Merk's now.
    fn replace(&mut self, prefix: &Prefix) {
        let left = self
            .merks
            .iter_mut()
            .find(|merk| merk.entry && !merk.live && merk.prefix == *prefix);
        if let Some(left) = left {
            left.entry = false;
            self.counts.replaced += 1;
        }
    }

    /// Enters a Merk in the directory and in the model.
    fn enter(&mut self, parent: Option<(usize, Seg)>, path: Vec<Seg>, prefix: Prefix) -> usize {
        let tree_type = {
            let len = self.below(4);
            self.bytes(len)
        };
        let above = parent.as_ref().map(|(index, _)| self.merks[*index].prefix);
        let host = parent
            .as_ref()
            .zip(above.as_ref())
            .map(|((_, seg), above)| Host {
                parent: above,
                key: &seg.0,
                position: seg.1,
            });
        let id = self
            .directory
            .add(&mut self.pages, &prefix, &tree_type, host)
            .expect("entered");
        assert!(!self.by_id.contains_key(&id), "an id is given out once");
        self.replace(&prefix);
        let index = self.merks.len();
        self.merks.push(Merk {
            path,
            prefix,
            id,
            root: None,
            tree_type,
            parent: parent.as_ref().map(|(index, _)| *index),
            children: BTreeMap::new(),
            live: true,
            entry: true,
            hosted: parent.is_some(),
        });
        if let Some((above, (key, position))) = parent {
            self.merks[above].children.insert((position, key), index);
        }
        self.by_id.insert(id, index);
        self.live.insert(prefix, index);
        self.seen.insert(prefix);
        self.counts.entered += 1;
        index
    }

    /// States roots to the directory, and takes them into the model.
    fn state(&mut self, changes: &[(usize, RootAt, bool)]) {
        // A Merk is stated by its prefix, by the address its root had, or
        // in both ways. A cell that became a root in another cell is also
        // said to be carried, from an address no root was at.
        let forms: Vec<(bool, bool, bool)> = changes
            .iter()
            .map(|(index, root, by_address)| {
                let was = self.merks[*index].root.as_ref().map(|(at, _)| *at);
                let now = root.as_ref().map(|(at, _)| *at);
                let carried = *by_address && was.is_some() && now.is_some();
                let both = carried && self.chance(20);
                let became = !carried && now.is_some() && was != now && self.chance(30);
                (carried, both, became)
            })
            .collect();
        let mut stated: Vec<RootChange<'_>> = Vec::new();
        for ((index, root, _), (carried, both, became)) in changes.iter().zip(forms) {
            let merk = &self.merks[*index];
            let by_prefix = named(&merk.prefix, root.as_ref().map(|(at, key)| (*at, &key[..])));
            let (was, now) = (merk.root.as_ref(), root.as_ref());
            if let (true, Some((from, _)), Some((to, _))) = (carried, was, now) {
                stated.push(RootChange::Carried {
                    from: *from,
                    to: *to,
                });
                self.counts.both_ways += u64::from(both);
            }
            if both || !carried {
                stated.push(by_prefix);
            }
            if let (true, Some((to, _))) = (became, now) {
                self.counts.became_roots += 1;
                let from = cell(900_000 + self.counts.became_roots as u32, 1);
                stated.insert(0, RootChange::Carried { from, to: *to });
            }
        }
        self.directory
            .set_roots(&mut self.pages, &stated)
            .expect("the roots are stated");
        for (index, root, _) in changes {
            self.merks[*index].root.clone_from(root);
        }
    }

    /// Gives the Merk `index` a root if it has none: a node of it hosts a
    /// Merk from here on.
    fn rooted(&mut self, index: usize) {
        if self.merks[index].root.is_none() {
            let root = (self.cell(), self.bytes(3));
            self.state(&[(index, Some(root), false)]);
        }
    }

    fn add(&mut self) {
        if self.chance(3) {
            // A Merk with no host, under a prefix of its own.
            let prefix: Prefix = self.bytes(32).try_into().expect("32 bytes");
            self.enter(None, Vec::new(), prefix);
            return;
        }
        let Some(above) = self.pick(|merk| merk.path.len() < 7) else {
            return;
        };
        let key = KEYS[self.below(KEYS.len())].to_vec();
        let seg = (key, [0, 0, 0, 1][self.below(4)]);
        let mut path = self.merks[above].path.clone();
        path.push(seg.clone());
        let prefix = self.prefix_below(above, &path);
        self.rooted(above);
        if self.live.contains_key(&prefix) {
            let pages = self.pages.pages.clone();
            let parent = self.merks[above].prefix;
            let host = Host {
                parent: &parent,
                key: &seg.0,
                position: seg.1,
            };
            let added = self
                .directory
                .add(&mut self.pages, &prefix, b"", Some(host));
            assert!(matches!(added, Err(DirectoryError::Exists { .. })));
            assert_eq!(self.pages.pages, pages);
            self.counts.refused += 1;
            return;
        }
        if self.seen.contains(&prefix) {
            self.counts.entered_again += 1;
        }
        self.enter(Some((above, seg)), path, prefix);
    }

    /// The prefix of the Merk at `path`, which is below the Merk `top` or
    /// that Merk itself. The Merks with no host have prefixes of their own,
    /// and what is below one is told apart by it.
    fn prefix_below(&self, top: usize, path: &[Seg]) -> Prefix {
        let mut at = top;
        while let Some(above) = self.merks[at].parent {
            at = above;
        }
        let mut whole = vec![(self.merks[at].prefix.to_vec(), 0xEE)];
        whole.extend_from_slice(path);
        match path.is_empty() {
            true => self.merks[at].prefix,
            false => prefix_of(&whole),
        }
    }

    fn roots(&mut self) {
        let mut changes: Vec<(usize, RootAt, bool)> = Vec::new();
        let mut given_up: Vec<CellAddress> = Vec::new();
        for _ in 0..1 + self.below(4) {
            let Some(index) = self.pick(|_| true) else {
                return;
            };
            if changes.iter().any(|(named, ..)| *named == index) {
                continue;
            }
            let merk = self.merks[index].clone();
            let dice = self.below(100);
            let root = match (&merk.root, dice) {
                // Its last node goes: no Merk is below it.
                (Some(_), 0..15) if merk.children.is_empty() => {
                    self.counts.emptied += 1;
                    None
                }
                // The root is carried to another cell: one that a root
                // of the same change has left, or any other.
                (Some((_, key)), 0..60) => {
                    self.counts.carried += 1;
                    let at = match given_up.is_empty() || self.chance(70) {
                        true => self.cell(),
                        false => {
                            self.counts.took_over += 1;
                            given_up.swap_remove(self.below(given_up.len()))
                        }
                    };
                    Some((at, key.clone()))
                }
                // Another node is the root, in the cell the root was in.
                (Some((at, _)), 60..70) => Some((*at, self.bytes(2))),
                _ => Some((self.cell(), self.bytes(2))),
            };
            let was = merk.root.as_ref().map(|(at, _)| *at);
            let now = root.as_ref().map(|(at, _)| *at);
            given_up.extend(was.filter(|was| Some(*was) != now));
            let by_address = matches!(dice, 0..60) && self.chance(70);
            changes.push((index, root, by_address));
            self.counts.roots += 1;
        }
        self.state(&changes);
        self.free_cells.extend(given_up);
    }

    /// Two roots change places, and a third takes a cell one of them left.
    fn swap(&mut self) {
        let Some(first) = self.pick(|merk| merk.root.is_some()) else {
            return;
        };
        let first_id = self.merks[first].id;
        let Some(second) = self.pick(|merk| merk.root.is_some() && merk.id != first_id) else {
            return;
        };
        let (a, b) = (
            self.merks[first].root.clone(),
            self.merks[second].root.clone(),
        );
        let (Some((a_at, a_key)), Some((b_at, b_key))) = (a, b) else {
            return;
        };
        let mut changes = vec![
            (first, Some((b_at, a_key)), self.chance(50)),
            (second, Some((a_at, b_key)), self.chance(50)),
        ];
        if self.chance(50) {
            changes.reverse();
        }
        self.state(&changes);
        self.counts.swapped += 1;
    }

    /// The Merks below `index` are in use no more.
    fn leave(&mut self, index: usize) {
        for below in self.subtree(index) {
            let merk = &mut self.merks[below];
            if merk.live {
                merk.live = false;
                self.live.remove(&merk.prefix);
                self.counts.left += u64::from(below != index);
            }
        }
    }

    fn unlink(&mut self) {
        let Some(index) = self.pick(|merk| merk.id != MerkId::FIRST) else {
            return;
        };
        let merk = self.merks[index].clone();
        let dropped = self
            .directory
            .unlink(&mut self.pages, &merk.prefix)
            .expect("dropped");
        assert_eq!(dropped.id, merk.id);
        assert_eq!(dropped.root, merk.root.as_ref().map(|(at, _)| *at));
        self.leave(index);
        let gone = &mut self.merks[index];
        (gone.entry, gone.hosted) = (false, false);
        if let Some(above) = merk.parent {
            let (key, position) = merk.path.last().expect("a hosted Merk has a path").clone();
            assert_eq!(
                self.merks[above].children.remove(&(position, key)),
                Some(index)
            );
        }
        self.drops.push(index);
        self.counts.dropped += 1;
    }

    fn clear(&mut self) {
        let Some(index) = self.pick(|_| true) else {
            return;
        };
        let merk = self.merks[index].clone();
        let dropped = self
            .directory
            .clear(&mut self.pages, &merk.prefix)
            .expect("cleared");
        let Some((root, _)) = &merk.root else {
            assert_eq!(dropped, None);
            return;
        };
        let dropped = dropped.expect("it had a root");
        assert_eq!((dropped.id, dropped.root), (merk.id, Some(*root)));
        let cleared = self.directory.open(&self.pages, &merk.prefix);
        let id = cleared.expect("a lookup").expect("it is entered").id();
        assert!(id > merk.id && !self.by_id.contains_key(&id));

        self.leave(index);
        let gone = &mut self.merks[index];
        (gone.entry, gone.hosted) = (false, false);
        let again = self.merks.len();
        self.merks.push(Merk {
            id,
            root: None,
            children: BTreeMap::new(),
            ..merk.clone()
        });
        if let Some(above) = merk.parent {
            let (key, position) = merk.path.last().expect("a hosted Merk has a path").clone();
            self.merks[above].children.insert((position, key), again);
        }
        self.by_id.insert(id, again);
        self.live.insert(merk.prefix, again);
        self.drops.push(index);
        self.counts.cleared += 1;
    }

    fn rekey(&mut self) {
        let Some(index) = self.pick(|merk| merk.id != MerkId::FIRST) else {
            return;
        };
        let moved = self.subtree(index);
        let ids: BTreeSet<MerkId> = moved.iter().map(|at| self.merks[*at].id).collect();
        let Some(above) = self.pick(|merk| merk.path.len() < 5 && !ids.contains(&merk.id)) else {
            return;
        };
        if moved.iter().any(|at| self.merks[*at].path.len() > 9) {
            return;
        }
        let key = KEYS[self.below(KEYS.len())].to_vec();
        let seg = (key, [0, 0, 1][self.below(3)]);
        let mut to = self.merks[above].path.clone();
        to.push(seg.clone());
        let prefix = self.prefix_below(above, &to);
        if self.live.contains_key(&prefix) {
            return;
        }
        self.rooted(above);

        // Where each Merk goes, by the steps from the moved Merk down to
        // it: what the directory asks for.
        let old = self.merks[index].clone();
        let places: Vec<(usize, Vec<Seg>, Prefix)> = moved
            .iter()
            .map(|at| {
                let below = &self.merks[*at].path[old.path.len()..];
                let path: Vec<Seg> = to.iter().chain(below).cloned().collect();
                let prefix = self.prefix_below(above, &path);
                (*at, path, prefix)
            })
            .collect();
        let by_steps: BTreeMap<&[Seg], Prefix> = places
            .iter()
            .map(|(_, path, prefix)| (&path[to.len()..], *prefix))
            .collect();
        let derive = |steps: &[Step<'_>]| {
            let below: Vec<Seg> = steps
                .iter()
                .map(|step| (step.key.to_vec(), step.position))
                .collect();
            by_steps[&below[..]]
        };
        let parent = self.merks[above].prefix;
        let host = Host {
            parent: &parent,
            key: &seg.0,
            position: seg.1,
        };
        let count = self
            .directory
            .rekey(&mut self.pages, &old.prefix, &prefix, host, derive);
        assert_eq!(count.expect("moved"), moved.len() as u64);

        for (at, ..) in &places {
            let prefix = self.merks[*at].prefix;
            self.live.remove(&prefix);
        }
        for (at, path, prefix) in places {
            self.replace(&prefix);
            let merk = &mut self.merks[at];
            (merk.path, merk.prefix) = (path, prefix);
            self.live.insert(prefix, at);
            self.seen.insert(prefix);
        }
        if let Some(was) = old.parent {
            let (key, position) = old.path.last().expect("a hosted Merk has a path").clone();
            assert_eq!(
                self.merks[was].children.remove(&(position, key)),
                Some(index)
            );
        }
        self.merks[above].children.insert((seg.1, seg.0), index);
        self.merks[index].parent = Some(above);
        self.merks[index].hosted = true;
        self.counts.moved += 1;
        self.counts.moved_merks += moved.len() as u64;
    }

    /// One step of the reclaimer: the records of one Merk that a dropped
    /// one left, or the record of a dropped Merk that left nothing else.
    /// It finds its way through the directory alone.
    fn reclaim(&mut self) {
        if self.drops.is_empty() {
            return;
        }
        let pick = self.below(self.drops.len());
        let dropped = self.merks[self.drops[pick]].id;
        let mut parent = dropped;
        loop {
            let mut children = Vec::new();
            let mut walk = self.directory.children(parent);
            while let Some(child) = walk.next(&self.pages).expect("a child record") {
                children.push(child);
            }
            if children.is_empty() {
                assert_eq!(
                    parent, dropped,
                    "a Merk with nothing below it is taken at once"
                );
                self.directory
                    .reclaimed(&mut self.pages, dropped)
                    .expect("all of it is reclaimed");
                let gone = self.drops.swap_remove(pick);
                self.free_cells
                    .extend(self.merks[gone].root.as_ref().map(|(at, _)| *at));
                self.counts.reclaimed_drops += 1;
                return;
            }
            let child = children.swap_remove(self.below(children.len()));
            let index = self.by_id[&child.id];
            if !self.merks[index]
                .children
                .values()
                .any(|at| self.merks[*at].hosted)
            {
                let keys = self.directory.left_by(&self.pages, parent, &child);
                let merk = &mut self.merks[index];
                assert_eq!(
                    keys.expect("records").len(),
                    1 + 2 * usize::from(merk.entry)
                        - usize::from(merk.entry && merk.root.is_none())
                );
                self.directory
                    .reclaim_child(&mut self.pages, parent, &child)
                    .expect("reclaimed");
                assert!(merk.hosted && !merk.live);
                (merk.entry, merk.hosted) = (false, false);
                self.free_cells
                    .extend(merk.root.as_ref().map(|(at, _)| *at));
                self.counts.reclaimed_merks += 1;
                return;
            }
            parent = child.id;
        }
    }

    fn step(&mut self) {
        let crowded = self.live.len() > 260;
        match self.below(100) {
            0..40 if !crowded => self.add(),
            0..58 => self.roots(),
            58..62 => self.swap(),
            62..70 => self.unlink(),
            70..73 => self.clear(),
            73..79 => self.rekey(),
            79..97 => self.reclaim(),
            _ => {
                let root = self.directory.root_page();
                let loaded = Directory::load(&self.pages, root, self.pages.config);
                self.directory = loaded.expect("a directory");
            }
        }
        let left = self.merks.iter().filter(|merk| merk.entry && !merk.live);
        self.counts.most_left = self.counts.most_left.max(left.count() as u64);
    }

    fn depth(&self, index: usize) -> usize {
        let mut depth = 0;
        let mut at = index;
        while let Some(above) = self.merks[at].parent {
            (depth, at) = (depth + 1, above);
        }
        depth
    }

    /// Compares the directory with the model.
    fn verify(&mut self) {
        let census = self.directory.check(&self.pages).expect("a directory");
        let left = self.merks.iter().filter(|merk| merk.entry && !merk.live);
        assert_eq!(census.live, self.live.len() as u64);
        assert_eq!(census.left_over, left.count() as u64);
        assert_eq!(census.dropped, self.drops.len() as u64);

        let levels = self.pages.levels(&self.directory);
        for (index, merk) in self.merks.iter().enumerate() {
            let (opened, read) = self
                .pages
                .reads(|| self.directory.open(&self.pages, &merk.prefix));
            let opened = opened.expect("a lookup");
            assert!(read.0 == levels && read.1 <= 1, "an open is one lookup");
            if !merk.live {
                // A Merk that is not in use is opened by nobody. An open
                // of its prefix gives the Merk that is in use there, if one
                // was entered since.
                let other = self.live.get(&merk.prefix).map(|at| self.merks[*at].id);
                assert_eq!(opened.map(|entry| entry.id()), other);
                let stored = self.directory.stored(&self.pages, &merk.prefix);
                let stored = stored.expect("a lookup").map(|entry| entry.id());
                assert_eq!(stored == Some(merk.id), merk.entry);
                continue;
            }
            assert_eq!(self.live.get(&merk.prefix), Some(&index));
            let entry = opened.expect("a Merk in use is opened");
            assert_eq!(entry.id(), merk.id);
            let root = entry.root().map(|root| (root.address, root.key.to_vec()));
            assert_eq!(root, merk.root);
            assert_eq!(entry.tree_type(), merk.tree_type);
            assert_eq!(entry.depth(), self.depth(index));
            match merk.parent {
                None => assert_eq!(entry.host(), None),
                Some(above) => {
                    let host = entry.host().expect("a host");
                    let (key, position) = merk.path.last().expect("a path");
                    assert_eq!(host.parent, &self.merks[above].prefix);
                    assert_eq!((host.key, host.position), (&key[..], *position));
                    assert_eq!(entry.parent_id(), Some(self.merks[above].id));
                }
            }
            if let Some((at, _)) = &merk.root {
                let owner = self.directory.root_owner(&self.pages, *at);
                assert_eq!(owner.expect("a lookup"), Some(merk.prefix));
            }
        }
        // By prefix, whatever was ever entered there.
        for prefix in &self.seen {
            let opened = self.directory.open(&self.pages, prefix).expect("a lookup");
            assert_eq!(opened.is_some(), self.live.contains_key(prefix));
        }
        // The entries that are stored are those of the Merks in use and
        // those that the reclaimer has not reached, and no other.
        let mut stored = BTreeSet::new();
        let mut entries = self.directory.entries();
        while let Some((prefix, entry)) = entries.next(&self.pages).expect("an entry") {
            assert!(stored.insert((prefix, entry.id())));
        }
        let expected: BTreeSet<(Prefix, MerkId)> = self
            .merks
            .iter()
            .filter(|merk| merk.entry)
            .map(|merk| (merk.prefix, merk.id))
            .collect();
        assert_eq!(stored, expected);
        let mut drops = BTreeSet::new();
        let mut walk = self.directory.drops();
        while let Some(dropped) = walk.next(&self.pages).expect("a record") {
            drops.insert(dropped.id);
        }
        let expected: BTreeSet<MerkId> = self.drops.iter().map(|at| self.merks[*at].id).collect();
        assert_eq!(drops, expected);
    }
}

#[test]
fn the_directory_agrees_with_a_model_and_no_open_returns_a_left_over_entry() {
    let mut total = Counts::default();
    let mut levels = 0;
    for seed in 1..=6 {
        let mut model = Model::new(seed);
        for step in 0..2_500 {
            model.step();
            if step % 5 == 4 {
                model.verify();
            }
        }
        model.verify();
        // Then the reclaimer comes to the end, and only the Merks in use
        // are left.
        while !model.drops.is_empty() {
            model.reclaim();
        }
        model.verify();
        let census = model.directory.check(&model.pages).expect("a directory");
        assert_eq!((census.left_over, census.dropped), (0, 0));
        levels = levels.max(model.pages.levels(&model.directory));
        let counts = &model.counts;
        for (sum, count) in [
            (&mut total.entered, counts.entered),
            (&mut total.entered_again, counts.entered_again),
            (&mut total.roots, counts.roots),
            (&mut total.carried, counts.carried),
            (&mut total.swapped, counts.swapped),
            (&mut total.emptied, counts.emptied),
            (&mut total.dropped, counts.dropped),
            (&mut total.left, counts.left),
            (&mut total.cleared, counts.cleared),
            (&mut total.moved, counts.moved),
            (&mut total.moved_merks, counts.moved_merks),
            (&mut total.replaced, counts.replaced),
            (&mut total.reclaimed_merks, counts.reclaimed_merks),
            (&mut total.reclaimed_drops, counts.reclaimed_drops),
            (&mut total.refused, counts.refused),
            (&mut total.both_ways, counts.both_ways),
            (&mut total.became_roots, counts.became_roots),
            (&mut total.took_over, counts.took_over),
        ] {
            *sum += count;
        }
        total.most_left = total.most_left.max(counts.most_left);
    }
    println!("six groves, a directory of up to {levels} levels: {total:?}");
    assert!(levels >= 2, "the directory had branches");
    assert!(total.entered > 3_000 && total.entered_again > 100);
    assert!(total.carried > 1_000 && total.swapped > 100 && total.emptied > 50);
    assert!(total.dropped > 500 && total.left > 500 && total.cleared > 50);
    assert!(total.moved > 100 && total.moved_merks > total.moved);
    assert!(
        total.replaced > 20,
        "{} left-over entries were replaced",
        total.replaced
    );
    assert!(total.reclaimed_merks > 300 && total.most_left > 20);
    assert!(total.both_ways > 50 && total.became_roots > 100 && total.took_over > 50);
}
