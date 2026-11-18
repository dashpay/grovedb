//! Crashes, on the simulated file system.
//!
//! A test runs its workload once for every operation after which a crash
//! can leave something new, with the power going off after that operation.
//! It then recovers the states that crash can leave, all of them where they
//! can be counted, and checks each one against the model.

use lignin_io::sim::{CrashPoint, SimConfig};

use super::{
    model::{check_recovered, Run, Step, RECOVERIES},
    *,
};
use crate::segment::tests::sim::{
    crash_states, sim_fs, OnFailure, Plans, EVERY_STATE, SOME_STATES,
};

/// Every step in the life of a table and its checkpoints: links of every
/// kind, a first checkpoint and later ones, a checkpoint that is discarded,
/// blocks moved to another segment and the old segment dropped, in one
/// session and across a restart. Then pages that are freed and trimmed out
/// of the table while the log is not durable through the commits that freed
/// them, one of them with a change that was never written back and one
/// whose id is used again, each followed by a checkpoint that covers no
/// more than the log has. The blocks are small enough for every state of
/// every crash to be counted.
pub(super) const TABLE_LIFE: [Step; 41] = [
    Step::Base(1),
    Step::Base(2),
    Step::Delta(1),
    Step::Checkpoint,
    Step::Delta(2),
    Step::Free(1),
    Step::Consolidate(2),
    Step::Checkpoint,
    Step::Segment,
    Step::Trim(2),
    Step::Relocate(2),
    Step::Trim(1),
    Step::Base(3),
    Step::Abandon,
    Step::Checkpoint,
    Step::DropUnused,
    Step::Restart,
    Step::Delta(3),
    Step::Checkpoint,
    Step::Sync,
    Step::Base(1),
    Step::Delta(1),
    Step::Checkpoint,
    Step::Restart,
    Step::Free(3),
    Step::Trim(3),
    Step::Checkpoint,
    Step::Delta(2),
    Step::Free(2),
    Step::Trim(2),
    Step::CheckpointBehind,
    Step::Change(1),
    Step::Free(1),
    Step::Trim(1),
    Step::Base(1),
    Step::CheckpointBehind,
    Step::Change(1),
    Step::SyncLog,
    Step::Delta(3),
    Step::CheckpointBehind,
    Step::WriteBack(1),
];

/// What the crashes of one workload came to.
#[derive(Debug, Default)]
struct Tally {
    /// Operations a crash was taken after.
    crash_points: usize,
    /// How many of them had every one of their states checked.
    exhaustive: usize,
    /// Crash plans recovered from.
    plans: usize,
    /// Distinct states among them, each of which was checked.
    states: usize,
    /// States that had the checkpoint of the last installation that
    /// returned.
    old: usize,
    /// States that had the checkpoint an installation was naming.
    new: usize,
}

/// Checks every state in `plans` that a crash of `fs` can leave now against
/// the model of `run`.
fn check_crash(
    fs: &SimVfs,
    run: &Run,
    plans: &Plans,
    tally: &mut Tally,
    place: &dyn Fn() -> String,
) {
    let image = fs.crash_image();
    let crash = crash_states(&image, plans);
    tally.crash_points += 1;
    tally.exhaustive += usize::from(crash.exhaustive);
    tally.plans += crash.plans;
    tally.states += crash.states.len();
    for (plan, state) in crash.states {
        let context = || format!("{}\n{image:?}\n{plan:?}", place());
        let _context = OnFailure(&context);
        if check_recovered(&state, &run.model) {
            tally.new += 1;
        } else {
            tally.old += 1;
        }
    }
}

/// Runs `workload` on a new file system once for every operation of it
/// after which a crash can leave something new. The power goes off after
/// that operation, and every state in `plans` that the crash can leave is
/// checked against the model of the run.
fn crash_everywhere(
    config: &SimConfig,
    seed: u64,
    plans: &Plans,
    workload: impl Fn(&mut Run) -> Result<(), PageTableError>,
) -> Tally {
    let mut tally = Tally::default();
    for point in 0.. {
        let fs = sim_fs(config.clone());
        fs.crash_at(CrashPoint::AfterChange(fs.stats().changes + point));
        let mut run = Run::new(&fs, seed);
        let result = workload(&mut run);
        if !fs.is_powered_off() {
            result.expect("the workload runs with the power on");
            run.check_session();
            break;
        }
        if let Err(error) = result {
            assert!(is_power_loss(&error), "crash point {point}: {error}");
        }
        check_crash(&fs, &run, plans, &mut tally, &|| {
            format!(
                "seed {seed}, crash point {point}, after {} steps",
                run.steps
            )
        });
    }
    tally
}

#[test]
fn every_state_of_a_crash_at_any_step_of_a_tables_life_has_a_checkpoint_and_its_blocks() {
    let tally = crash_everywhere(&SimConfig::new(11), 11, &EVERY_STATE, |run| {
        run.script(&TABLE_LIFE)
    });
    let met = RECOVERIES.get();
    println!("{tally:?}, {met:?}");
    assert!(tally.crash_points >= 180, "{tally:?}");
    assert_eq!(tally.exhaustive, tally.crash_points, "{tally:?}");
    assert!(tally.old > 0 && tally.new > 0, "{tally:?}");
    assert!(met.forgotten > 0 && met.dropped > 0, "{met:?}");
}

#[test]
fn a_crash_tears_writes_at_any_sixteen_bytes_of_a_tables_life() {
    let config = SimConfig {
        sector_size: 16,
        ..SimConfig::new(12)
    };
    let tally = crash_everywhere(&config, 12, &SOME_STATES, |run| run.script(&TABLE_LIFE));
    println!("{tally:?}");
    assert!(tally.old > 0 && tally.new > 0, "{tally:?}");
}

#[test]
fn a_crash_at_any_operation_of_a_drawn_workload_leaves_what_the_model_allows() {
    let mut all = Tally::default();
    for seed in [25, 30, 34] {
        let tally = crash_everywhere(&SimConfig::new(seed), seed, &SOME_STATES, |run| run.run(90));
        println!("seed {seed}: {tally:?}");
        assert!(tally.crash_points >= 100, "{tally:?}");
        all.old += tally.old;
        all.new += tally.new;
    }
    let met = RECOVERIES.get();
    println!("{met:?}");
    assert!(all.old > 0 && all.new > 0, "{all:?}");
    assert!(met.forgotten > 0 && met.dropped > 0, "{met:?}");
}

#[test]
fn a_database_goes_on_through_one_crash_after_another() {
    let mut tally = Tally::default();
    let mut installed = 0;
    let mut lost_commits = 0;
    for seed in [31, 32, 33, 34] {
        let mut fs = sim_fs(SimConfig::new(seed));
        let mut run = Run::new(&fs, seed);
        for crash in 0..25 {
            let more = 1 + (seed + 7 * crash) % 61;
            fs.crash_at(CrashPoint::AfterChange(fs.stats().changes + more));
            let target = run.steps + 400;
            if let Err(error) = run.run(target) {
                assert!(is_power_loss(&error), "seed {seed}, crash {crash}: {error}");
            }
            assert!(fs.is_powered_off());
            check_crash(&fs, &run, &SOME_STATES, &mut tally, &|| {
                format!("seed {seed}, crash {crash}")
            });

            let image = fs.crash_image();
            let plan = image.sample(crash);
            let new = check_recovered(&image.recover(&plan).unwrap(), &run.model);
            let left = image.recover(&plan).unwrap();
            let lost = run.model.version - run.model.log_durable;
            let landing = run.model.log_durable + (seed + 3 * crash) % (lost + 1);
            lost_commits += usize::from(landing < run.model.version);
            run.crashed(&left, new, landing);
            installed += usize::from(run.model.acked.is_some());
            fs = left;
        }
        let target = run.steps + 60;
        run.run(target).expect("the workload goes on");
        run.check_session();
    }
    let met = RECOVERIES.get();
    println!(
        "{tally:?}, {installed} crashes with a checkpoint in force, {lost_commits} that lost \
         commits, {met:?}"
    );
    assert!(met.forgotten > 0 && met.dropped > 0, "{met:?}");
    assert_eq!(tally.crash_points, 100);
    assert!(lost_commits >= 20, "{lost_commits}");
    assert!(tally.old > 0 && tally.new > 0, "{tally:?}");
    assert!(installed >= 50, "{installed}");
}
