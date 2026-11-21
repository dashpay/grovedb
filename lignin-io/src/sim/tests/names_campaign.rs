//! What a crash can leave of changes of names, as numbers that are the same
//! for the same seed: on every machine, and whichever way the simulator
//! finds the changes that a change builds on.

use super::*;
use crate::sim::trace::Fnv;

const FILES: [&str; 6] = ["/a", "/b", "/d/a", "/d/b", "/e/a", "/d/in/a"];
const DIRS: [&str; 5] = ["/d", "/e", "/d/in", "/e/in", "/"];

struct States {
    digest: u64,
    states: u64,
    kept_with_more: u64,
    most_of_one_name: usize,
}

/// A workload on a few names that its seed decides alone, with a crash
/// after every step: the two ends, each change lost alone, a file system
/// that writes in order, three plans drawn from the seed, every plan where
/// there are few, each change of names kept alone and three sets of them
/// as they fall, which a plan completes with what they build on. The
/// digest is of the changes of names each plan keeps, of the file system it
/// leaves and of its crash record.
fn crash_states(seed: u64, steps: u64) -> States {
    let fs = sim(seed);
    let mut rng = SimRng::derive(seed, 3_000, 0);
    let mut digest = Fnv::new();
    let mut found = States {
        digest: 0,
        states: 0,
        kept_with_more: 0,
        most_of_one_name: 0,
    };
    for _ in 0..steps {
        let mut pick = |of: &[&'static str]| p(of[rng.below(of.len() as u64) as usize]);
        let (file, other) = (pick(&FILES), pick(&FILES));
        let (dir, other_dir) = (pick(&DIRS), pick(&DIRS));
        let byte = [rng.next_u64() as u8];
        let _ = match rng.below(18) {
            0..=3 => fs
                .open(file, OpenMode::Write(Create::IfMissing))
                .and_then(|opened| opened.write_at(&byte, 0)),
            4 | 5 => fs.rename(file, other),
            6 => fs.hard_link(file, other),
            7 | 8 => fs.remove_file(file),
            9..=11 => fs.create_dir(dir),
            12 => fs.remove_dir(dir),
            13 | 14 => fs.rename(dir, other_dir),
            15 => fs.sync_dir(dir),
            16 => fs.sync_dir(dir.parent().unwrap_or(dir)),
            _ => fs
                .open(file, OpenMode::Read)
                .and_then(|opened| opened.sync_data()),
        };

        let image = fs.crash_image();
        let mut of_one_name: BTreeMap<&str, usize> = BTreeMap::new();
        for name in image.names() {
            *of_one_name.entry(&name.path).or_default() += 1;
        }
        let most = of_one_name.values().copied().max().unwrap_or(0);
        found.most_of_one_name = found.most_of_one_name.max(most);

        let mut plans = vec![image.lose_all(), image.keep_all()];
        plans.extend(image.holes());
        plans.extend(image.prefixes());
        plans.extend((0..3).map(|draw| image.sample(draw)));
        plans.extend(image.every_plan(64).into_iter().flatten());
        let names = 0..image.names().len();
        plans.extend(names.clone().map(|name| names_only(vec![name])));
        for _ in 0..3 {
            let some = names.clone().filter(|_| rng.below(3) == 0);
            plans.push(names_only(some.collect()));
        }
        for plan in plans {
            let kept = image.names_kept(&plan);
            found.kept_with_more += u64::from(kept.len() > plan.names.len());
            let left = image.recover(&plan).unwrap();
            digest.u64(kept.len() as u64);
            kept.iter().for_each(|name| digest.u64(*name as u64));
            digest.u64(left.fingerprint());
            digest.u64(left.trace_digest());
            found.states += 1;
        }
    }
    found.digest = digest.0;
    found
}

/// The crash states of three seeds. They are the same on every machine, and
/// they do not depend on how the simulator finds the changes that a change
/// builds on: a change to that leaves these numbers as they are, or has to
/// say which crash states it changed.
#[test]
fn the_crash_states_of_a_seed_are_what_they_were() {
    let pinned: [(u64, u64, u64); 4] = [
        (1, 40, 0x813F_B897_649C_DD85),
        (1, 400, 0xA00F_BD04_F7BA_5839),
        (2, 400, 0xDE83_E80A_51E7_8486),
        (0x00C0_FFEE, 400, 0x922D_D6D5_F7D4_6A4F),
    ];
    let mut differ = Vec::new();
    for (seed, steps, digest) in pinned {
        if cfg!(miri) && steps > 40 {
            continue;
        }
        let found = crash_states(seed, steps);
        println!(
            "seed {seed:#x}, {steps} steps: digest {:#018x}, {} crash states, {} of them with \
             a change kept that the plan did not list, up to {} unsynced changes of one name",
            found.digest, found.states, found.kept_with_more, found.most_of_one_name
        );
        if found.digest != digest {
            differ.push((seed, steps, found.digest));
        }
        if steps > 40 {
            assert!(found.states > 15_000, "{} states", found.states);
            assert!(found.kept_with_more > 1_000, "{}", found.kept_with_more);
            assert!(found.most_of_one_name >= 4, "{}", found.most_of_one_name);
        }
    }
    assert!(differ.is_empty(), "the digests differ: {differ:x?}");
}
