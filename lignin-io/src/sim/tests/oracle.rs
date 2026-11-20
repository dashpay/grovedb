//! The crash states of small cases, every one of them, against the set the
//! contract allows. Each test builds that set in its own way, from the words
//! of the contract and of the simulator's documentation, not from the
//! simulator's code.

use super::*;

const SCRIPTS: u64 = if cfg!(miri) { 3 } else { 150 };

fn contents(
    image: &CrashImage,
    path: &str,
    plans: impl IntoIterator<Item = CrashPlan>,
) -> BTreeSet<Vec<u8>> {
    plans
        .into_iter()
        .map(|plan| read(&recover(image, &plan), path))
        .collect()
}

/// The plans a harness tries first.
fn common_plans(image: &CrashImage) -> Vec<CrashPlan> {
    let sampled = (0..if cfg!(miri) { 4 } else { 40 }).map(|draw| image.sample(draw));
    image
        .prefixes()
        .chain(image.holes())
        .chain(image.reorders())
        .chain(sampled)
        .collect()
}

/// Every plan together gives exactly the allowed states, and the plans a
/// harness tries first give none but those.
#[track_caller]
fn assert_leaves(image: &CrashImage, path: &str, allowed: &BTreeSet<Vec<u8>>, case: &str) {
    assert_eq!(&contents(image, path, every_plan(image)), allowed, "{case}");
    let common = contents(image, path, common_plans(image));
    assert!(common.is_subset(allowed), "{case}");
}

/// "Any subset of them, in any order, and a write possibly only in part,
/// torn at any byte": with sectors of one byte, a byte that unsynced writes
/// overwrote is the durable one or the one any of those writes put there,
/// whatever the bytes around it are.
#[test]
fn overwrites_leave_exactly_the_states_the_contract_allows() {
    for seed in 0..SCRIPTS.min(12) {
        let mut rng = SimRng::new(seed);
        let fs = sim_with_sectors(seed, 1);
        let durable = b"abcde";
        let file = create_durable(&fs, "/file", durable);
        let mut writes = Vec::new();
        for write in 0..2 + seed % 2 {
            let len = 1 + rng.below(if cfg!(miri) { 1 } else { 2 });
            let offset = rng.below(durable.len() as u64 - len + 1);
            let data: Vec<u8> = (0..len)
                .map(|byte| b'A' + (write * 8 + byte) as u8)
                .collect();
            file.write_at(&data, offset).unwrap();
            writes.push((offset as usize, data));
        }

        let mut allowed = BTreeSet::from([Vec::new()]);
        for (at, durable_byte) in durable.iter().enumerate() {
            let mut bytes = BTreeSet::from([*durable_byte]);
            for (offset, data) in &writes {
                if (*offset..offset + data.len()).contains(&at) {
                    bytes.insert(data[at - offset]);
                }
            }
            allowed = allowed
                .iter()
                .flat_map(|prefix| {
                    bytes.iter().map(move |byte| {
                        let mut content = prefix.clone();
                        content.push(*byte);
                        content
                    })
                })
                .collect();
        }
        assert_leaves(
            &fs.crash_image(),
            "/file",
            &allowed,
            &format!("seed {seed}"),
        );
    }
}

/// "Each lands where it was made, an append at the offset it returned. Where
/// a length survives without the bytes written below it, those bytes read as
/// zeros": with sectors of one byte, a file that was appended to ends
/// anywhere from its durable end to its last end, and each byte past the
/// durable end is the appended one or zero.
#[test]
fn appends_leave_exactly_the_states_the_contract_allows() {
    for seed in 0..SCRIPTS.min(8) {
        let mut rng = SimRng::new(seed);
        let fs = sim_with_sectors(seed, 1);
        drop(create_durable(&fs, "/log", b"ab"));
        let log = fs.open(p("/log"), OpenMode::Append(Create::Never)).unwrap();
        let mut appended = Vec::new();
        for append in 0..if cfg!(miri) { 1 } else { 2 } {
            let len = 1 + rng.below(2);
            let data: Vec<u8> = (0..len)
                .map(|byte| b'A' + (append * 8 + byte) as u8)
                .collect();
            assert_eq!(log.append(&data).unwrap(), 2 + appended.len() as u64);
            appended.extend(data);
        }

        let mut allowed = BTreeSet::new();
        for len in 0..=appended.len() {
            for zeroed in 0..1u32 << len {
                let mut content = b"ab".to_vec();
                for (at, byte) in appended[..len].iter().enumerate() {
                    content.push(if zeroed >> at & 1 == 1 { 0 } else { *byte });
                }
                allowed.insert(content);
            }
        }
        assert_leaves(&fs.crash_image(), "/log", &allowed, &format!("seed {seed}"));
    }
}

/// One unsynced write on storage that writes a sector whole or not at all:
/// the file has the length it had, or ends inside the write at a sector
/// boundary or at the write's end. Up to that length, each sector's part of
/// the write is there whole or not at all, and where it is not, the file has
/// what it had, or zeros past its old end.
#[test]
fn one_write_tears_exactly_as_whole_sectors_allow() {
    for seed in 0..SCRIPTS {
        let mut rng = SimRng::new(seed);
        let sector = 2 + rng.below(3);
        let fs = sim_with_sectors(seed, sector);
        let durable: Vec<u8> = (0..rng.below(9)).map(|byte| b'a' + byte as u8).collect();
        let file = create_durable(&fs, "/file", &durable);
        let offset = rng.below(durable.len() as u64 + 3);
        let data: Vec<u8> = (0..1 + rng.below(6))
            .map(|byte| b'A' + byte as u8)
            .collect();
        file.write_at(&data, offset).unwrap();
        let (old, start, end) = (durable.len(), offset as usize, offset as usize + data.len());

        let mut lens = BTreeSet::from([old]);
        lens.extend(
            (start + 1..=end)
                .filter(|len| *len > old && ((*len as u64).is_multiple_of(sector) || *len == end)),
        );
        let first = start as u64 / sector;
        let last = (end as u64 - 1) / sector;
        let mut allowed = BTreeSet::new();
        for len in lens {
            for written in 0..1u32 << (last - first + 1) {
                let mut content = durable.clone();
                content.resize(len, 0);
                for (at, byte) in content.iter_mut().enumerate() {
                    let in_write = (start..end).contains(&at);
                    if in_write && written >> (at as u64 / sector - first) & 1 == 1 {
                        *byte = data[at - start];
                    }
                }
                allowed.insert(content);
            }
        }
        let case = format!("seed {seed}: {durable:?}, {data:?} at {offset}, sectors of {sector}");
        assert_leaves(&fs.crash_image(), "/file", &allowed, &case);
    }
}

/// What the documentation of `Tear` and `CrashPlan` says a plan does to a
/// file, done one byte at a time.
fn applied(durable: &[u8], image: &CrashImage, plan: &CrashPlan) -> Vec<u8> {
    let sector = image.sector_size();
    let mut file = durable.to_vec();
    for survivor in &plan.changes {
        let change = &image.changes()[survivor.change];
        let (offset, len) = match change.kind {
            FileChangeKind::SetLen { len } => {
                file.resize(len as usize, 0);
                continue;
            }
            FileChangeKind::Write { offset, len } => (offset, len),
        };
        let grown = survivor.tear.as_ref().map_or(offset + len, |tear| tear.len);
        if file.len() < grown as usize {
            file.resize(grown as usize, 0);
        }
        for (at, byte) in (offset..).zip(change.data()) {
            let its_sector = at / sector - offset / sector;
            let survives = survivor.tear.as_ref().is_none_or(|tear| {
                tear.sectors
                    .iter()
                    .any(|sectors| sectors.contains(&its_sector))
            });
            if survives && (at as usize) < file.len() {
                file[at as usize] = *byte;
            }
        }
    }
    file
}

#[test]
fn a_plan_does_to_a_file_what_its_words_say() {
    for seed in 0..SCRIPTS * 4 {
        let mut rng = SimRng::new(seed);
        let sector = 1 + rng.below(6);
        let fs = sim_with_sectors(seed, sector);
        let durable: Vec<u8> = (0..rng.below(20)).map(|byte| b'a' + byte as u8).collect();
        let file = create_durable(&fs, "/file", &durable);
        for change in 0..1 + rng.below(5) {
            if rng.below(4) == 0 {
                file.set_len(rng.below(30)).unwrap();
                continue;
            }
            let data: Vec<u8> = (0..1 + rng.below(12))
                .map(|byte| b'A' + (change * 12 + byte) as u8)
                .collect();
            file.write_at(&data, rng.below(30)).unwrap();
        }

        let image = fs.crash_image();
        let mut plans = common_plans(&image);
        for _ in 0..if cfg!(miri) { 4 } else { 30 } {
            let mut changes: Vec<Survivor> = Vec::new();
            for (place, change) in image.changes().iter().enumerate() {
                if rng.below(3) == 0 {
                    continue;
                }
                let tear = match change.kind {
                    FileChangeKind::Write { offset, len } if rng.below(2) == 0 => {
                        let end = offset + len;
                        let lens: Vec<u64> = (offset + 1..=end)
                            .filter(|at| at.is_multiple_of(sector) || *at == end)
                            .chain([0])
                            .collect();
                        let sectors = (0..change.sectors).filter(|_| rng.below(2) == 0);
                        Some(Tear {
                            sectors: sectors.map(|sector| sector..sector + 1).collect(),
                            len: lens[rng.below(lens.len() as u64) as usize],
                        })
                    }
                    _ => None,
                };
                let at = rng.below(changes.len() as u64 + 1) as usize;
                let survivor = Survivor {
                    change: place,
                    tear,
                };
                changes.insert(at, survivor);
            }
            plans.push(CrashPlan {
                names: Vec::new(),
                changes,
            });
        }
        for plan in plans {
            assert_eq!(
                read(&recover(&image, &plan), "/file"),
                applied(&durable, &image, &plan),
                "seed {seed}, sectors of {sector}: {plan:?} of {:?}",
                image.changes()
            );
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Named {
    File(u64),
    Dir(u64),
}

type Name = (u64, String);

struct Made {
    op: u64,
    rename: bool,
    removed_dir: Option<u64>,
    effects: Vec<(Name, Option<Named>)>,
}

/// What the contract and the simulator's documentation say about names, kept
/// as a list of what was done.
///
/// The contract: "a name that was created, renamed, linked or removed, and a
/// directory that was created or removed, is durable once sync_dir returns
/// Ok for the directory that holds the name", and "of the changes of names
/// not yet synced a crash may keep any subset in which each change comes
/// with the earlier changes it builds on".
#[derive(Default)]
struct Names {
    now: BTreeMap<Name, Named>,
    made: Vec<Made>,
    synced: Vec<(u64, u64)>,
    written: BTreeMap<u64, Vec<u8>>,
    durable: BTreeMap<u64, Vec<u8>>,
    tags: u64,
}

impl Names {
    fn dir(&self, path: &str) -> Option<u64> {
        let mut dir = 0;
        for name in path.split('/').filter(|name| !name.is_empty()) {
            match self.now.get(&(dir, name.to_owned())) {
                Some(Named::Dir(next)) => dir = *next,
                _ => return None,
            }
        }
        Some(dir)
    }

    fn name(&self, path: &str) -> Name {
        let (dir, name) = path.rsplit_once('/').unwrap();
        (self.dir(dir).unwrap(), name.to_owned())
    }

    fn file(&self, path: &str) -> u64 {
        match self.now[&self.name(path)] {
            Named::File(tag) => tag,
            Named::Dir(_) => panic!("{path} is a directory"),
        }
    }

    fn record(&mut self, op: u64, effects: Vec<(Name, Option<Named>)>) {
        let removed_dir = match (&effects[..], self.now.get(&effects[0].0)) {
            ([(_, None)], Some(Named::Dir(dir))) => Some(*dir),
            ([(_, Some(Named::Dir(_))), _], Some(Named::Dir(replaced))) => Some(*replaced),
            _ => None,
        };
        for (name, named) in &effects {
            match named {
                Some(named) => self.now.insert(name.clone(), *named),
                None => self.now.remove(name),
            };
        }
        self.made.push(Made {
            op,
            rename: effects.len() == 2,
            removed_dir,
            effects,
        });
    }

    fn tag(&mut self) -> u64 {
        self.tags += 1;
        self.tags
    }

    fn is_synced(&self, made: &Made) -> bool {
        made.effects.iter().all(|((dir, _), _)| {
            self.synced
                .iter()
                .any(|(synced, at)| synced == dir && *at > made.op)
        })
    }

    /// The operations that gave `dir`, and every directory above it, the
    /// names they had before operation `op`: of each, the last that made it
    /// or renamed it.
    fn placed_above(&self, mut dir: u64, op: u64) -> Vec<u64> {
        let mut ops = Vec::new();
        while dir != 0 {
            let places_it =
                |made: &&Made| made.op < op && made.effects[0].1 == Some(Named::Dir(dir));
            let placed = self.made.iter().rev().find(places_it).unwrap();
            ops.push(placed.op);
            dir = placed.effects[0].0 .0;
        }
        ops
    }

    /// Whether no change in `kept` is there without the changes it builds
    /// on: the earlier changes of its names, for the removal of a directory
    /// and for a rename that replaces one the changes that emptied it, and
    /// for a rename what gave the directories of its names, and those above
    /// them, the names they had when it was made.
    fn holds_together(&self, kept: &BTreeSet<u64>) -> bool {
        let is_kept = |made: &Made| kept.contains(&made.op);
        self.made.iter().filter(|made| is_kept(made)).all(|made| {
            let earlier = || self.made.iter().filter(|earlier| earlier.op < made.op);
            let touches = |other: &Made, name: &Name| {
                other.effects.iter().any(|(touched, _)| touched == name)
            };
            let is_in = |other: &Made, dir: u64| {
                other.effects.iter().any(|((in_dir, _), _)| *in_dir == dir)
            };
            let names = made
                .effects
                .iter()
                .all(|(name, _)| earlier().filter(|other| touches(other, name)).all(is_kept));
            let emptied = made
                .removed_dir
                .is_none_or(|dir| earlier().filter(|other| is_in(other, dir)).all(is_kept));
            let dirs = !made.rename
                || made.effects.iter().all(|((dir, _), _)| {
                    let placed = self.placed_above(*dir, made.op);
                    placed.iter().all(|op| kept.contains(op))
                });
            names && emptied && dirs
        })
    }

    fn after_a_crash_that_keeps(&self, kept: &BTreeSet<u64>) -> Tree {
        let mut names = BTreeMap::new();
        for made in self.made.iter().filter(|made| kept.contains(&made.op)) {
            for (name, named) in &made.effects {
                match named {
                    Some(named) => names.insert(name.clone(), *named),
                    None => names.remove(name),
                };
            }
        }
        let mut tree = Tree::new();
        self.walk(&names, 0, "", &mut tree);
        tree
    }

    fn walk(&self, names: &BTreeMap<Name, Named>, dir: u64, path: &str, tree: &mut Tree) {
        for ((_, name), named) in names.range((dir, String::new())..(dir + 1, String::new())) {
            let path = format!("{path}{name}");
            match named {
                Named::File(tag) => {
                    tree.insert(path, Some(self.durable[tag].clone()));
                }
                Named::Dir(tag) => {
                    self.walk(names, *tag, &format!("{path}/"), tree);
                    tree.insert(path, None);
                }
            }
        }
    }
}

#[test]
fn changes_of_names_leave_exactly_the_states_the_documentation_allows() {
    const FILES: [&str; 6] = ["/a", "/b", "/d/a", "/d/b", "/e/a", "/d/e/a"];
    const DIRS: [&str; 6] = ["/", "/d", "/e", "/d/e", "/e/d", "/d/e/d"];

    let (mut compared, mut renamed_dirs) = (0, 0);
    for seed in 0..SCRIPTS {
        let mut rng = SimRng::new(seed);
        let fs = sim(seed);
        let mut names = Names::default();
        let file = |rng: &mut SimRng| FILES[rng.below(FILES.len() as u64) as usize];
        let dir = |rng: &mut SimRng| DIRS[rng.below(DIRS.len() as u64) as usize];
        for path in ["/d", "/e"] {
            let op = fs.stats().ops + 1;
            fs.create_dir(p(path)).unwrap();
            let tag = names.tag();
            names.record(op, vec![(names.name(path), Some(Named::Dir(tag)))]);
        }

        for _ in 0..if cfg!(miri) { 16 } else { 40 } {
            let op = fs.stats().ops + 1;
            match rng.below(26) {
                16..=25 => {
                    let below_the_root = &DIRS[1..];
                    let pick = |rng: &mut SimRng| {
                        below_the_root[rng.below(below_the_root.len() as u64) as usize]
                    };
                    let (from, to) = (pick(&mut rng), pick(&mut rng));
                    if fs.rename(p(from), p(to)).is_ok() {
                        let (from, to) = (names.name(from), names.name(to));
                        let moved = names.now[&from];
                        if names.now.get(&to) != Some(&moved) {
                            names.record(op, vec![(to, Some(moved)), (from, None)]);
                            renamed_dirs += 1;
                        }
                    }
                }
                0..=3 => {
                    let path = file(&mut rng);
                    let Ok(created) = fs.open(p(path), OpenMode::Write(Create::New)) else {
                        continue;
                    };
                    let tag = names.tag();
                    let content = tag.to_string().into_bytes();
                    created.write_at(&content, 0).unwrap();
                    created.sync_data().unwrap();
                    names.written.insert(tag, content.clone());
                    names.durable.insert(tag, content);
                    names.record(op, vec![(names.name(path), Some(Named::File(tag)))]);
                }
                4 | 13 => {
                    let path = file(&mut rng);
                    if fs.remove_file(p(path)).is_ok() {
                        names.record(op, vec![(names.name(path), None)]);
                    }
                }
                5..=7 => {
                    let (from, to) = (file(&mut rng), file(&mut rng));
                    if fs.rename(p(from), p(to)).is_ok() {
                        let (from, to) = (names.name(from), names.name(to));
                        let moved = names.now[&from];
                        if names.now.get(&to) != Some(&moved) {
                            names.record(op, vec![(to, Some(moved)), (from, None)]);
                        }
                    }
                }
                8 => {
                    let (original, link) = (file(&mut rng), file(&mut rng));
                    if fs.hard_link(p(original), p(link)).is_ok() {
                        let linked = names.now[&names.name(original)];
                        names.record(op, vec![(names.name(link), Some(linked))]);
                    }
                }
                9 | 10 => {
                    let path = dir(&mut rng);
                    if fs.create_dir(p(path)).is_ok() {
                        let tag = names.tag();
                        names.record(op, vec![(names.name(path), Some(Named::Dir(tag)))]);
                    }
                }
                11 => {
                    let path = dir(&mut rng);
                    if fs.remove_dir(p(path)).is_ok() {
                        names.record(op, vec![(names.name(path), None)]);
                    }
                }
                12 => {
                    let path = dir(&mut rng);
                    if fs.sync_dir(p(path)).is_ok() {
                        names.synced.push((names.dir(path).unwrap(), op));
                    }
                }
                _ => {
                    let path = file(&mut rng);
                    let Ok(opened) = fs.open(p(path), OpenMode::Append(Create::Never)) else {
                        continue;
                    };
                    let tag = names.file(path);
                    opened.append(b"+").unwrap();
                    names.written.get_mut(&tag).unwrap().push(b'+');
                    if rng.below(2) == 0 {
                        opened.sync_data().unwrap();
                        names.durable.insert(tag, names.written[&tag].clone());
                    }
                }
            }
            check(&fs);
        }

        let image = fs.crash_image();
        let (synced, unsynced): (Vec<&Made>, Vec<&Made>) =
            names.made.iter().partition(|made| names.is_synced(made));
        let is_unsynced = |op: &u64| unsynced.iter().any(|made| made.op == *op);
        assert!(
            image.names().iter().all(|name| is_unsynced(&name.op)),
            "seed {seed}"
        );

        if unsynced.len() <= 11 {
            let mut allowed = BTreeSet::new();
            for mask in 0..1u32 << unsynced.len() {
                let chosen = (0..unsynced.len()).filter(|at| mask >> at & 1 == 1);
                let kept: BTreeSet<u64> = chosen
                    .map(|at| unsynced[at].op)
                    .chain(synced.iter().map(|made| made.op))
                    .collect();
                if names.holds_together(&kept) {
                    allowed.insert(names.after_a_crash_that_keeps(&kept));
                }
            }
            let simulated: BTreeSet<Tree> = name_subsets(&image)
                .map(|kept| tree(&recover(&image, &names_only(kept))))
                .collect();
            assert_eq!(simulated, allowed, "seed {seed}: {:?}", image.names());
            compared += 1;
        }

        let kept_all = recover(&image, &image.keep_all());
        assert_eq!(kept_all.fingerprint(), fs.fingerprint());
        assert_eq!(tree(&kept_all), tree(&fs));
        names.durable = names.written.clone();
        let everything: BTreeSet<u64> = names.made.iter().map(|made| made.op).collect();
        assert_eq!(tree(&kept_all), names.after_a_crash_that_keeps(&everything));
    }
    assert!(compared * 2 >= SCRIPTS, "{compared} of {SCRIPTS}");
    assert!(
        cfg!(miri) || renamed_dirs >= SCRIPTS / 2,
        "{renamed_dirs} directories renamed, {compared} scripts compared"
    );
}

/// "A crash keeps a rename whole or not at all", as it shows from outside: a
/// file whose name is durable, and that is renamed, by its own name or with
/// a directory above it, and never removed or replaced, has a name in every
/// state a crash can leave.
#[test]
fn a_file_that_is_only_renamed_has_a_name_after_every_crash() {
    const FILES: [&str; 7] = ["/a", "/b", "/c", "/d/a", "/d/b", "/e/a", "/d/e/a"];
    const DIRS: [&str; 6] = ["/", "/d", "/e", "/d/e", "/e/d", "/d/e/d"];
    let mut renamed_dirs = 0;

    for seed in 0..SCRIPTS {
        let mut rng = SimRng::new(seed);
        let fs = sim(seed);
        let file = |rng: &mut SimRng| p(FILES[rng.below(FILES.len() as u64) as usize]);
        let dir = |rng: &mut SimRng| p(DIRS[rng.below(DIRS.len() as u64) as usize]);
        let mut files = 0;
        fs.create_dir(p("/d")).unwrap();
        fs.create_dir(p("/e")).unwrap();
        for _ in 0..if cfg!(miri) { 20 } else { 50 } {
            match rng.below(14) {
                10..=13 => {
                    let (from, to) = (dir(&mut rng), dir(&mut rng));
                    renamed_dirs += u64::from(fs.rename(from, to).is_ok() && from != to);
                }
                0 | 1 => {
                    if let Ok(created) = fs.open(file(&mut rng), OpenMode::Write(Create::New)) {
                        files += 1;
                        created.write_at(&[files], 0).unwrap();
                        created.sync_data().unwrap();
                    }
                }
                2..=5 => {
                    let (from, to) = (file(&mut rng), file(&mut rng));
                    if fs.open(to, OpenMode::Read).is_err() {
                        let _ = fs.rename(from, to);
                    }
                }
                6 | 7 => drop(fs.create_dir(dir(&mut rng))),
                8 => drop(fs.remove_dir(dir(&mut rng))),
                _ => drop(fs.sync_dir(dir(&mut rng))),
            }
            check(&fs);
        }

        let image = fs.crash_image();
        let files_in =
            |fs: &SimVfs| -> BTreeSet<Vec<u8>> { tree(fs).into_values().flatten().collect() };
        let durable = files_in(&recover(&image, &image.lose_all()));
        let count = image.names().len();
        for tried in 0..if cfg!(miri) { 8 } else { 200u64 } {
            let mask = match count {
                0..=7 => tried % (1 << count),
                _ => rng.next_u64(),
            };
            let kept = (0..count).filter(|name| mask >> name & 1 == 1).collect();
            let left = files_in(&recover(&image, &names_only(kept)));
            assert!(
                durable.is_subset(&left),
                "seed {seed}: keeping {mask:#b} of {:?} leaves {left:?} of {durable:?}",
                image.names()
            );
        }
    }
    assert!(
        cfg!(miri) || renamed_dirs >= SCRIPTS / 2,
        "{renamed_dirs} directories renamed"
    );
}
