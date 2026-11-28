//! Crashes, on the simulated file system.
//!
//! A test runs its work once for every operation after which a crash can
//! leave something new, with the power going off after that operation. It
//! then recovers the states that crash can leave, all of them where they can
//! be counted, and checks each one against the record of the run.

use std::collections::BTreeSet;

use lignin_io::sim::{CrashPoint, SimConfig, SimVfs};

use super::{
    run::{check_recovered, put, Op, Run},
    sim::*,
    *,
};

/// A database's first two sessions, through every step of a value-log
/// file's life: values of commits and of a rollback, overwritten, deleted
/// and moved cells, a snapshot that pins what a later commit replaces,
/// cleaning passes, files dropped and deleted, a checkpoint of the pages
/// taken at one moment and one taken across a commit and a pass,
/// and a second session that cleans a file of the first.
///
/// In the first session the transactions write to file 1 and then, from the
/// rollback on, to file 2, and the cleaner writes to file 3.
pub(super) fn life(run: &mut Run) -> Result<(), ValueStoreError> {
    run.open()?;
    run.commit(&[
        put(1, 10, 40),
        put(1, 11, 700),
        put(2, 20, 90),
        put(2, 21, 33),
        put(3, 30, 1500),
    ])?;
    run.flush()?;
    let pinned = run.pin();
    run.commit(&[
        put(1, 10, 50),
        Op::Delete { key: 20 },
        Op::Move { key: 21, to: 3 },
    ])?;
    run.db().rotate()?;
    run.transact(&[put(4, 40, 64)], false)?;
    run.commit(&[put(4, 41, 80)])?;

    run.clean(file_id(1))?;
    run.drop_empty()?;
    run.flush()?;
    run.drop_empty()?;
    run.checkpoint()?;
    run.db().begin_checkpoint()?;
    run.db().copy_pages(2);
    run.commit(&[Op::Delete { key: 11 }, put(5, 50, 20)])?;
    run.unpin(pinned);
    run.drop_empty()?;
    run.flush()?;

    run.db().rotate()?;
    run.db().copy_pages(1);
    run.clean(file_id(2))?;
    run.db().finish_checkpoint()?;
    run.flush()?;
    run.drop_empty()?;
    run.commit(&[put(6, 60, 10), Op::Move { key: 30, to: 6 }])?;
    run.flush()?;

    run.end_session();
    run.open()?;
    run.commit(&[put(1, 10, 900), Op::Delete { key: 41 }])?;
    run.clean(file_id(3))?;
    run.flush()?;
    run.drop_empty()?;
    run.commit(&[put(2, 22, 70)])?;
    run.flush()
}

/// What the crashes of one workload came to.
#[derive(Debug, Default)]
struct CrashTally {
    /// Operations a crash was taken after.
    crash_points: usize,
    /// How many of them had every one of their states checked.
    exhaustive: usize,
    /// Crash plans recovered from.
    plans: usize,
    /// Distinct states among them, each of which was checked.
    states: usize,
    /// States that recovered to the newest version known durable.
    at_durable: usize,
    /// States that recovered to a later version.
    past_durable: usize,
    /// The versions that were recovered.
    versions: BTreeSet<u64>,
    /// States in which a file the workload had deleted was gone.
    with_a_deleted_file: usize,
}

/// Runs `workload` on a new file system once for every operation of it after
/// which a crash can leave something new. The power goes off after that
/// operation, and every state in `plans` that the crash can leave is checked
/// against the record of the run.
fn crash_everywhere(
    config: &SimConfig,
    seed: u64,
    plans: &Plans,
    workload: impl Fn(&mut Run) -> Result<(), ValueStoreError>,
) -> CrashTally {
    let mut tally = CrashTally::default();
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
            assert!(lost_power(&error), "crash point {point}: {error}");
        }
        check_crash(&fs, &run, plans, &mut tally, &|| {
            format!("seed {seed}, crash point {point}")
        });
    }
    tally
}

/// Checks every state in `plans` that a crash of `fs` can leave now against
/// the record of `run`.
fn check_crash(
    fs: &SimVfs,
    run: &Run,
    plans: &Plans,
    tally: &mut CrashTally,
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
        let db = check_recovered(&state, run);
        let version = db.pages.latest();
        tally.versions.insert(version);
        if version == run.durable {
            tally.at_durable += 1;
        } else {
            tally.past_durable += 1;
        }
        let on_disk = db.files_on_disk();
        let last = on_disk.last().map_or(0, |id| id.get());
        tally.with_a_deleted_file += usize::from(on_disk.len() < last as usize);
    }
}

const MOST_STATES: Plans = Plans {
    every_plan_limit: 20_000,
    samples: 24,
};

#[test]
fn every_state_of_a_crash_at_any_step_of_a_value_logs_life_recovers_pages_and_counts_that_agree() {
    let tally = crash_everywhere(&SimConfig::new(11), 11, &MOST_STATES, life);
    println!("{tally:?}");
    assert!(tally.crash_points >= 150, "{tally:?}");
    assert!(tally.exhaustive * 10 >= tally.crash_points * 9, "{tally:?}");
    assert!(tally.at_durable > 0 && tally.past_durable > 0, "{tally:?}");
    assert_eq!(tally.versions, (0..=7).collect(), "{tally:?}");
    assert!(tally.with_a_deleted_file > 0, "{tally:?}");
}

#[test]
fn a_crash_tears_writes_at_any_sixteen_bytes_of_a_value_logs_life() {
    let config = SimConfig {
        sector_size: 16,
        ..SimConfig::new(12)
    };
    let tally = crash_everywhere(&config, 12, &SOME_STATES, life);
    println!("{tally:?}");
    assert!(tally.at_durable > 0 && tally.past_durable > 0, "{tally:?}");
    assert_eq!(tally.versions, (0..=7).collect(), "{tally:?}");
}

/// The part of a file's life in which moves empty it: the cleaner's copies,
/// the rewrites, and the deletion of the file, first too early to be allowed
/// and then after the rewrites are durable.
fn moves_empty_a_file(run: &mut Run) -> Result<(), ValueStoreError> {
    run.open()?;
    run.commit(&[put(1, 10, 300), put(2, 20, 300), put(3, 30, 40)])?;
    run.db().rotate()?;
    run.flush()?;
    run.clean(file_id(1))?;
    run.drop_empty()?;
    run.commit(&[put(1, 11, 30)])?;
    run.drop_empty()?;
    run.flush()?;
    run.drop_empty()?;
    run.commit(&[Op::Delete { key: 20 }])?;
    run.flush()
}

#[test]
fn a_crash_after_moves_emptied_a_file_never_recovers_a_page_that_points_into_a_deleted_file() {
    let tally = crash_everywhere(&SimConfig::new(13), 13, &MOST_STATES, moves_empty_a_file);
    println!("{tally:?}");
    assert!(tally.exhaustive * 10 >= tally.crash_points * 9, "{tally:?}");
    assert!(tally.with_a_deleted_file > 0, "{tally:?}");
    assert_eq!(tally.versions, (0..=3).collect(), "{tally:?}");
}

#[test]
fn a_crash_at_any_operation_of_a_drawn_workload_leaves_what_the_record_allows() {
    let mut all = CrashTally::default();
    for seed in [21, 22, 23] {
        let tally = crash_everywhere(&SimConfig::new(seed), seed, &SOME_STATES, |run| {
            run.open()?;
            run.run(70)
        });
        println!("seed {seed}: {tally:?}");
        assert!(tally.crash_points >= 150, "{tally:?}");
        all.at_durable += tally.at_durable;
        all.past_durable += tally.past_durable;
        all.with_a_deleted_file += tally.with_a_deleted_file;
    }
    assert!(all.at_durable > 0 && all.past_durable > 0, "{all:?}");
    assert!(all.with_a_deleted_file > 0, "{all:?}");
}

#[test]
fn a_database_goes_on_through_one_crash_after_another() {
    let mut tally = CrashTally::default();
    let (mut moved, mut dropped) = (0, 0);
    for seed in [31, 32, 33, 34] {
        let mut fs = sim_fs(SimConfig::new(seed));
        let mut run = Run::new(&fs, seed);
        for crash in 0..25 {
            let more = 1 + (seed + 7 * crash) % 37;
            fs.crash_at(CrashPoint::AfterChange(fs.stats().changes + more));
            let target = run.steps + 400;
            let result = run.open().and_then(|()| run.run(target));
            let error = result.expect_err("the power goes off");
            assert!(lost_power(&error), "seed {seed}, crash {crash}: {error}");
            check_crash(&fs, &run, &SOME_STATES, &mut tally, &|| {
                format!("seed {seed}, crash {crash}")
            });

            let image = fs.crash_image();
            let left = image.recover(&image.sample(crash)).unwrap();
            run.crashed(&left);
            fs = left;
        }
        run.open().expect("the database opens");
        let target = run.steps + 60;
        run.run(target).expect("the workload goes on");
        run.check_session();
        moved += run.tally.moved;
        dropped += run.tally.dropped;
    }
    println!("{tally:?}, {moved} values moved, {dropped} files dropped");
    assert_eq!(tally.crash_points, 100);
    assert!(tally.at_durable > 0 && tally.past_durable > 0, "{tally:?}");
    assert!(moved > 0 && dropped > 0);
}
