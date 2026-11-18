//! "The same seed reproduces the same failure schedule."

use std::{
    sync::{Condvar, Mutex},
    thread,
};

use super::*;
use crate::sim::trace::Fnv;

const STEPS: u64 = 600;
const NAMES: [&str; 5] = ["/a", "/b", "/dir/a", "/dir/b", "/LOCK"];
const DIRS: [&str; 2] = ["/", "/dir"];

struct Campaign {
    schedule: u64,
    faults: u64,
    settled: usize,
    power_cuts: usize,
    lost_and_kept: (usize, usize),
}

/// A workload that its seed decides alone, on a file system that fails and
/// crashes as that seed says. `schedule` digests everything the simulator
/// did along the way: every operation and how it ended, every fault and what
/// it left, and every crash state.
fn campaign(seed: u64) -> Campaign {
    let config = SimConfig {
        sector_size: 16,
        trace_ops: true,
        faults: FaultOdds {
            eio: Chance::new(1, 12),
            enospc: Chance::new(1, 12),
            ..FaultOdds::NONE
        },
        ..SimConfig::new(seed)
    };
    let mut fs = SimVfs::new(config);
    let mut rng = SimRng::derive(seed, 1_000, 0);
    let mut files: Vec<Box<dyn VfsFile>> = Vec::new();
    let mut schedule = Fnv::new();
    let mut campaign = Campaign {
        schedule: 0,
        faults: 0,
        settled: 0,
        power_cuts: 0,
        lost_and_kept: (0, 0),
    };

    for step in 0..STEPS {
        let name = p(NAMES[rng.below(5) as usize]);
        let other = p(NAMES[rng.below(5) as usize]);
        let dir = p(DIRS[rng.below(2) as usize]);
        let data: Vec<u8> = (0..rng.below(60)).map(|_| rng.next_u64() as u8).collect();
        let at = rng.below(files.len().max(1) as u64) as usize;
        let _ = match rng.below(40) {
            0..=3 => {
                let create = [
                    Create::IfMissing,
                    Create::IfMissing,
                    Create::New,
                    Create::Never,
                ];
                let create = create[rng.below(4) as usize];
                let mode = match rng.below(5) {
                    0 => OpenMode::Read,
                    1 | 2 => OpenMode::Write(create),
                    _ => OpenMode::Append(create),
                };
                fs.open(name, mode).map(|file| files.push(file))
            }
            4 => fs.rename(name, other),
            5 => fs.hard_link(name, other),
            6 => fs.remove_file(name),
            7 => fs.create_dir(p("/dir")),
            8 => match rng.below(3) {
                0 => fs.remove_dir(p("/dir")),
                _ => fs.create_dir(p("/dir")),
            },
            9 => fs.read_dir(dir).map(drop),
            10 => fs.sync_dir(dir),
            11 => fs.lock(name).map(drop),
            12 if !files.is_empty() => {
                files.swap_remove(at);
                Ok(())
            }
            13 => {
                let synced = files.iter().filter(|file| file.sync_data().is_ok());
                schedule.u64(synced.count() as u64);
                Ok(())
            }
            kind => match files.get(at) {
                None => Ok(()),
                Some(file) => match kind % 9 {
                    0 => file.read_at(&mut [0; 32], rng.below(100)).map(drop),
                    1..=3 => file.write_at(&data, rng.below(100)),
                    4 | 5 => file.append(&data).map(drop),
                    6 => file.set_len(rng.below(120)),
                    7 => match rng.below(2) {
                        0 => file.sync_data(),
                        _ => file.sync_all(),
                    },
                    _ => file.len().map(drop),
                },
            },
        };

        if step % 50 == 10 && rng.below(2) == 0 {
            fs.crash_at(CrashPoint::AfterChange(fs.stats().changes + rng.below(12)));
        }
        if step % 50 == 49 {
            let trace = fs.trace();
            schedule.u64(digest(&trace));
            schedule.u64(fs.fingerprint());
            campaign.faults += fs.stats().faults;
            for event in &trace {
                match event {
                    Event::Settled { .. } => campaign.settled += 1,
                    Event::PowerOff { .. } => campaign.power_cuts += 1,
                    Event::Crash(record) => {
                        let kept = |change: &&ChangeFate| change.fate != Fate::Lost;
                        let kept = record.changes.iter().filter(kept).count();
                        campaign.lost_and_kept.0 += record.changes.len() - kept;
                        campaign.lost_and_kept.1 += kept;
                    }
                    Event::Op(_) => {}
                }
            }
            files.clear();
            check(&fs);
            fs = fs.crash(step);
            check(&fs);
        }
    }
    schedule.u64(fs.trace_digest());
    schedule.u64(fs.fingerprint());
    campaign.schedule = schedule.0;
    campaign
}

#[test]
fn the_same_seed_reproduces_the_same_schedule() {
    let mut schedules = BTreeSet::new();
    for seed in [0, 1, 2, 3, 0xDEAD_BEEF, u64::MAX] {
        if cfg!(miri) && seed > 1 {
            continue;
        }
        let (first, second) = (campaign(seed), campaign(seed));
        assert_eq!(first.schedule, second.schedule, "seed {seed}");

        schedules.insert(first.schedule);
    }
    assert_eq!(schedules.len(), if cfg!(miri) { 2 } else { 6 });
}

/// The schedules of three seeds, as they were when this test was written.
/// They are the same on every machine: 32 or 64 bits, either byte order.
/// A change to the simulator that changes them changes what every recorded
/// failing seed means, and has to say so.
#[test]
fn the_schedule_of_a_seed_is_the_same_everywhere() {
    let pinned = [
        (1, 0xE892_F724_8FCA_57AB),
        (2, 0x1C0C_3C45_8B96_4D9F),
        (0x00C0_FFEE, 0x9735_4C25_5856_277B),
    ];
    let mut seen = (0, 0);
    for (seed, schedule) in pinned {
        let campaign = campaign(seed);
        assert_eq!(
            campaign.schedule, schedule,
            "seed {seed:#x} gave schedule {:#018x}",
            campaign.schedule
        );
        seen.0 += campaign.settled;
        seen.1 += campaign.power_cuts;
    }
    assert!(seen.0 > 0 && seen.1 > 0, "{seen:?}");
}

fn in_turns(seed: u64) -> u64 {
    const THREADS: u64 = 3;
    let fs = SimVfs::new(SimConfig {
        trace_ops: true,
        faults: FaultOdds {
            eio: Chance::new(1, 6),
            ..FaultOdds::NONE
        },
        ..SimConfig::new(seed)
    });
    let turn = Mutex::new(0);
    let passed = Condvar::new();
    thread::scope(|scope| {
        for thread in 0..THREADS {
            let (fs, turn, passed) = (&fs, &turn, &passed);
            scope.spawn(move || {
                let path = format!("/file-{thread}");
                let mut file = None;
                for round in 0..12 {
                    let mut now = turn.lock().unwrap();
                    while *now % THREADS != thread {
                        now = passed.wait(now).unwrap();
                    }
                    match &file {
                        None => file = fs.open(p(&path), OpenMode::Append(Create::IfMissing)).ok(),
                        Some(file) if round % 3 == 0 => drop(file.sync_data()),
                        Some(file) => drop(file.append(path.as_bytes())),
                    }
                    *now += 1;
                    passed.notify_all();
                }
            });
        }
    });
    assert!(fs.stats().faults > 0);
    fs.trace_digest()
}

#[test]
fn threads_that_take_turns_get_the_same_schedule() {
    assert_eq!(in_turns(5), in_turns(5));
    assert_ne!(in_turns(5), in_turns(6));
}

#[test]
fn a_rule_with_a_path_hits_the_same_call_whatever_other_threads_do() {
    for seed in 0..if cfg!(miri) { 2 } else { 20 } {
        let fs = sim(seed);
        fs.add_rule(
            FaultRule::new(Fault::Eio)
                .on(OpSet::SYNCS)
                .at("/mine")
                .skip(2),
        );
        thread::scope(|scope| {
            let noise = scope.spawn(|| {
                let file = create(&fs, "/noise", b"");
                for _ in 0..40 {
                    file.write_at(b"noise", 0).unwrap();
                    file.sync_data().unwrap();
                }
            });
            let file = create(&fs, "/mine", b"");
            let synced: Vec<bool> = (0..5)
                .map(|_| {
                    file.write_at(b"mine", 0).unwrap();
                    thread::yield_now();
                    file.sync_all().is_ok()
                })
                .collect();
            assert_eq!(synced, [true, true, false, false, false]);
            noise.join().unwrap();
        });
        assert_eq!(fs.stats().faults, 1);
    }
}
