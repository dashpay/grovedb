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
