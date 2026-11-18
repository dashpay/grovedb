//! The crash states of small cases, every one of them, against the set the
//! contract allows, which each test builds in its own way from the words of
//! the contract.

use super::*;

const SCRIPTS: u64 = if cfg!(miri) { 3 } else { 150 };

fn simulated(image: &CrashImage, path: &str) -> BTreeSet<Vec<u8>> {
    all_change_plans(image)
        .iter()
        .map(|plan| read(&recover(image, plan), path))
        .collect()
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
        assert_eq!(
            simulated(&fs.crash_image(), "/file"),
            allowed,
            "seed {seed}"
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
        assert_eq!(simulated(&fs.crash_image(), "/log"), allowed, "seed {seed}");
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
        assert_eq!(
            simulated(&fs.crash_image(), "/file"),
            allowed,
            "seed {seed}: {durable:?}, {data:?} at {offset}, sectors of {sector}"
        );
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
    effects: Vec<(Name, Option<Named>)>,
}

/// What the contract says about names, kept as a list of what was done: "a
/// name that was created, renamed, linked or removed, and a directory that
/// was created or removed, is durable once sync_dir returns Ok for the
/// directory that holds the name", "of the changes of names not yet synced a
/// crash may keep any subset", and "a crash keeps a rename whole or not at
/// all".
#[derive(Default)]
struct Names {
    now: BTreeMap<Name, Named>,
    made: Vec<Made>,
    synced: Vec<(u64, u64)>,
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

    fn record(&mut self, op: u64, effects: Vec<(Name, Option<Named>)>) {
        for (name, named) in &effects {
            match named {
                Some(named) => self.now.insert(name.clone(), *named),
                None => self.now.remove(name),
            };
        }
        self.made.push(Made { op, effects });
    }

    fn tag(&mut self) -> u64 {
        self.tags += 1;
        self.tags
    }

    fn is_durable(&self, made: &Made) -> bool {
        made.effects.iter().all(|((dir, _), _)| {
            self.synced
                .iter()
                .any(|(synced, at)| synced == dir && *at > made.op)
        })
    }

    fn after_a_crash_that_keeps(&self, kept: &BTreeSet<u64>) -> Tree {
        let mut names = BTreeMap::new();
        for made in &self.made {
            if !self.is_durable(made) && !kept.contains(&made.op) {
                continue;
            }
            for (name, named) in &made.effects {
                match named {
                    Some(named) => names.insert(name.clone(), *named),
                    None => names.remove(name),
                };
            }
        }
        fn walk(names: &BTreeMap<Name, Named>, dir: u64, path: &str, tree: &mut Tree) {
            for ((_, name), named) in names.range((dir, String::new())..(dir + 1, String::new())) {
                let path = format!("{path}{name}");
                match named {
                    Named::File(tag) => {
                        tree.insert(path, Some(tag.to_string().into_bytes()));
                    }
                    Named::Dir(tag) => {
                        walk(names, *tag, &format!("{path}/"), tree);
                        tree.insert(path, None);
                    }
                }
            }
        }
        let mut tree = Tree::new();
        walk(&names, 0, "", &mut tree);
        tree
    }
}

#[test]
fn changes_of_names_leave_exactly_the_states_the_contract_allows() {
    const FILES: [&str; 6] = ["/a", "/b", "/d/a", "/d/b", "/e/a", "/d/e/a"];
    const DIRS: [&str; 4] = ["/", "/d", "/e", "/d/e"];

    let mut recovered = 0;
    for seed in 0..SCRIPTS {
        let mut rng = SimRng::new(seed);
        let fs = sim(seed);
        let mut names = Names::default();
        let file = |rng: &mut SimRng| FILES[rng.below(FILES.len() as u64) as usize];
        let dir = |rng: &mut SimRng| DIRS[rng.below(DIRS.len() as u64) as usize];

        for _ in 0..if cfg!(miri) { 16 } else { 60 } {
            let op = fs.stats().ops + 1;
            match rng.below(14) {
                0..=3 => {
                    let path = file(&mut rng);
                    let Ok(created) = fs.open(p(path), OpenMode::Write(Create::New)) else {
                        continue;
                    };
                    let tag = names.tag();
                    created.write_at(tag.to_string().as_bytes(), 0).unwrap();
                    created.sync_data().unwrap();
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
                _ => {
                    let path = dir(&mut rng);
                    if fs.sync_dir(p(path)).is_ok() {
                        names.synced.push((names.dir(path).unwrap(), op));
                    }
                }
            }
            check(&fs);
        }

        let image = fs.crash_image();
        assert!(image.changes().is_empty());
        let unsynced: Vec<u64> = names
            .made
            .iter()
            .filter(|made| !names.is_durable(made))
            .map(|made| made.op)
            .collect();
        let listed: BTreeSet<u64> = image.names().iter().map(|name| name.op).collect();
        assert!(listed.iter().all(|op| unsynced.contains(op)), "seed {seed}");

        let subsets = 1u64 << unsynced.len();
        let tried = if cfg!(miri) { 4 } else { 256 };
        for subset in 0..subsets.min(tried) {
            let mask = match subsets <= tried {
                true => subset,
                false => rng.below(subsets),
            };
            let kept: BTreeSet<u64> = (0..unsynced.len())
                .filter(|at| mask >> at & 1 == 1)
                .map(|at| unsynced[at])
                .collect();
            let plan = CrashPlan {
                names: (0..image.names().len())
                    .filter(|name| kept.contains(&image.names()[*name].op))
                    .collect(),
                changes: Vec::new(),
            };
            assert_eq!(
                tree(&recover(&image, &plan)),
                names.after_a_crash_that_keeps(&kept),
                "seed {seed}, keeping {kept:?} of {:?}",
                image.names()
            );
            recovered += 1;
        }

        let everything: BTreeSet<u64> = unsynced.into_iter().collect();
        let kept_all = recover(&image, &image.keep_all());
        assert_eq!(tree(&kept_all), names.after_a_crash_that_keeps(&everything));
        assert_eq!(kept_all.fingerprint(), fs.fingerprint());
        assert_eq!(tree(&kept_all), tree(&fs));
    }
    assert!(recovered >= SCRIPTS);
}
