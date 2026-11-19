//! The log under the crash model of the file layer: whatever state a crash
//! leaves, at any point of a session and of the sessions after it, the log
//! reads as a run of the items that were appended, with every durable one.

use lignin_io::sim::SimConfig;

use super::*;

const FEW_STATES: sim::Plans = sim::Plans {
    every_plan_limit: 400,
    samples: 12,
};

fn policies() -> [SyncPolicy; 3] {
    [
        SyncPolicy::Never,
        SyncPolicy::EveryCommit,
        SyncPolicy::Interval(std::time::Duration::ZERO),
    ]
}

#[test]
fn every_state_a_crash_can_leave_reads_as_the_items_that_were_appended() {
    let total = crash_states_of_drawn_sessions(1..=4, 100);
    assert!(total.states > 100_000, "{total:?}");
    assert!(total.appended > 250 && total.turns > 15 && total.dropped > 0);
}

/// The same with more seeds and longer sessions: over a million states.
/// `cargo test -p lignin-store --lib wal::tests::crash -- --ignored`
#[test]
#[ignore = "takes minutes"]
fn every_state_a_crash_can_leave_of_long_sessions() {
    let total = crash_states_of_drawn_sessions(1..=12, 260);
    assert!(total.states > 1_000_000, "{total:?}");
    assert!(total.appended > 1500 && total.turns > 100 && total.dropped > 30);
}

fn crash_states_of_drawn_sessions(seeds: std::ops::RangeInclusive<u64>, steps: u32) -> Stats {
    let mut total = Stats::default();
    for seed in seeds {
        let sector_size = [512, 64, 4096, 1, 16, 512][seed as usize % 6];
        let options = LogOptions {
            target_file_len: 1200 + 300 * (seed % 5),
            sync: policies()[seed as usize % 4 % 3],
        };
        let config = SimConfig {
            sector_size,
            ..SimConfig::new(seed)
        };
        let mut run = Run::new(config, options, FEW_STATES);
        run.check_crash_states();
        for _ in 0..steps {
            run.step();
            run.check_crash_states();
        }
        assert_eq!(
            (run.stats.other_errors, run.stats.not_appended),
            (0, 0),
            "nothing fails without a fault: {}",
            run.at()
        );
        let stats = run.stats;
        println!("seed {seed}, sectors of {sector_size}: {stats:?}");
        total.states += stats.states;
        total.appended += stats.appended;
        total.turns += stats.turns;
        total.dropped += stats.dropped;
        total.syncs += stats.syncs;
    }
    println!("{total:?}");
    total
}

#[test]
fn a_database_goes_on_through_one_crash_after_another() {
    let mut total = Stats::default();
    for seed in 1..=24 {
        let options = LogOptions {
            target_file_len: 2000,
            sync: policies()[seed as usize % 4 % 3],
        };
        let config = SimConfig {
            sector_size: [512, 64, 1][seed as usize % 3],
            ..SimConfig::new(100 + seed)
        };
        let mut run = Run::new(config, options, FEW_STATES);
        for life in 0..12 {
            for _ in 0..(5 + run.rng.below(25)) {
                run.step();
            }
            if life % 4 == 3 {
                // A process that exits loses nothing.
                run.restart();
            } else {
                run.crash(life);
            }
            // The versions the crash took are given out again, and no
            // later recovery brings a lost commit back: the states a
            // second crash can leave hold the new items.
            run.check_crash_states();
            for _ in 0..3 {
                run.step();
                run.check_crash_states();
            }
        }
        run.restart();
        let read = Run::read(&run.fs).unwrap();
        run.model.check(&read, true).unwrap();
        // Versions go up by one along what is left, whatever was lost.
        let versions: Vec<u64> = read
            .iter()
            .filter_map(|item| item.version)
            .map(|version| version.get())
            .collect();
        assert!(versions.windows(2).all(|pair| pair[1] == pair[0] + 1));
        let stats = run.stats;
        println!("seed {seed}: {stats:?}");
        assert_eq!((stats.other_errors, stats.not_appended), (0, 0));
        total.crashes += stats.crashes;
        total.lost_in_a_crash += stats.lost_in_a_crash;
        total.states += stats.states;
        total.appended += stats.appended;
    }
    println!("{total:?}");
    assert!(
        total.crashes >= 200 && total.lost_in_a_crash > 300,
        "{total:?}"
    );
    assert!(total.states > 30_000 && total.appended > 3000, "{total:?}");
}

/// Two sessions, the second of which begins where the first ended with no
/// crash, with the power cut after operation `cut` of the file system.
/// Returns the run, and the numbers of the operations of the second
/// session.
fn a_session_after_another(seed: u64, cut: Option<u64>) -> (Run, std::ops::Range<u64>) {
    let config = sim::traced(seed, [64, 512][seed as usize % 2]);
    let mut run = Run::new(config, small_files(1500), FEW_STATES);
    // The first session ends with items in two files that no sync covers.
    for step in 0..16 {
        run.append();
        if step == 5 {
            run.sync();
        }
        if step == 9 {
            run.roll();
        }
    }
    run.note_durable();
    assert!(run.model.durable < run.model.items.len());
    let from = run.fs.stats().ops;
    if let Some(cut) = cut {
        run.fs.crash_at(lignin_io::sim::CrashPoint::AfterOp(cut));
    }

    // The next session reads the log, opens it, rolls for the first time
    // and goes on. With the power off, every call of it fails.
    run.db = None;
    if let Ok(read) = Run::read(&run.fs) {
        run.model.check(&read, true).unwrap();
        run.model.recovered(&read, false);
        run.open();
        for _ in 0..3 {
            run.append();
        }
        run.sync();
        run.roll();
        run.append();
        run.note_durable();
    }
    let to = run.fs.stats().ops;
    (run, from..to)
}

#[test]
fn a_power_cut_at_any_operation_of_a_first_roll_leaves_a_log_that_reads() {
    use lignin_io::sim::{Event, OpKind};
    let mut total = Stats::default();
    let mut cuts = 0;
    for seed in 1..=4 {
        let (whole, ops) = a_session_after_another(seed, None);
        assert_eq!(whole.stats.other_errors, 0);
        // The session wrote again the files whose ends no manifest vouched
        // for, so the cuts fall between its copy, its rename and the sync
        // of the directory too.
        let renames = whole
            .fs
            .trace()
            .into_iter()
            .filter(|event| {
                matches!(event, Event::Op(op)
                    if op.op > ops.start && op.kind == OpKind::Rename && op.path.ends_with(".log.tmp"))
            })
            .count();
        assert!(renames >= 1, "{renames} files written again");

        for (cut, draw) in ops.flat_map(|cut| [(cut, cut), (cut, !cut)]) {
            let (mut run, _) = a_session_after_another(seed, Some(cut));
            assert!(run.fs.is_powered_off());
            cuts += 1;
            // Whatever the cut left, and whatever of it a crash keeps: the
            // log reads, and holds what was durable.
            if draw == cut {
                run.check_crash_states();
            }
            // The session after it goes on from one of those states: it
            // opens, rolls, takes an item and makes it durable.
            run.crash(draw);
            assert!(run.db.is_some(), "the directory opens after cut {cut}");
            let before = run.stats;
            run.append();
            if run.stats.appended == before.appended {
                run.append();
            }
            run.sync();
            run.note_durable();
            assert_eq!(run.stats.appended, before.appended + 1, "cut {cut}");
            assert_eq!(run.stats.syncs, before.syncs + 1, "cut {cut}");
            assert_eq!(run.model.durable, run.model.items.len(), "cut {cut}");
            run.check_crash_states();
            let read = Run::read(&run.fs).unwrap();
            run.model.check(&read, true).unwrap();
            total.states += run.stats.states;
            total.appended += run.stats.appended;
            total.unsettled += run.stats.unsettled;
            total.other_errors += run.stats.other_errors;
            total.lost_in_a_crash += run.stats.lost_in_a_crash;
        }
    }
    println!("{cuts} cuts: {total:?}");
    assert!(
        cuts > 500 && total.states > 15_000,
        "{cuts} cuts, {total:?}"
    );
}
