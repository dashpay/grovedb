//! Crashes, on the simulated file system.
//!
//! A test runs its work once for every operation after which a crash can
//! leave something new, with the power going off after that operation. It
//! then recovers the states that crash can leave, all of them where they can
//! be counted, and checks each one.

use std::collections::BTreeSet;

use lignin_io::sim::{CrashImage, CrashPlan, CrashPoint, OpKind, OpRecord, SimConfig, SimVfs};

use super::{
    model::{check_recovered, first_manifest, with_another_log, Run, SEGMENT_LIFE},
    sim::*,
    *,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Left {
    Old,
    New,
}

const REPLACEMENT: [&str; 6] = [
    "nothing",
    "the temporary file is created",
    "the temporary file is written",
    "the temporary file is synced",
    "the temporary file is renamed",
    "the directory is synced",
];

/// The step of a replacement that `last`, the last operation to take place,
/// belongs to; which manifests a crash right after it may leave; and whether
/// the replacement then returns `Ok`.
fn left_after(last: Option<&OpRecord>) -> (&'static str, &'static [Left], bool) {
    let Some(last) = last else {
        return (REPLACEMENT[0], &[Left::Old], false);
    };
    let temporary = last.path.ends_with(ManifestStore::TEMP_FILE_NAME);
    match last.kind {
        OpKind::RemoveFile if temporary => {
            ("the stale temporary file is removed", &[Left::Old], false)
        }
        OpKind::Open if temporary => (REPLACEMENT[1], &[Left::Old], false),
        OpKind::Write if temporary => (REPLACEMENT[2], &[Left::Old], false),
        OpKind::SyncAll if temporary => (REPLACEMENT[3], &[Left::Old], false),
        OpKind::Rename if temporary => (REPLACEMENT[4], &[Left::Old, Left::New], false),
        OpKind::SyncDir if last.path == DB => (REPLACEMENT[5], &[Left::New], true),
        _ => panic!("a replacement is not known to make this operation: {last}"),
    }
}

/// One state a crash left.
struct Crashed<'a> {
    image: &'a CrashImage,
    plan: &'a CrashPlan,
    state: SimVfs,
    /// The manifest in it, if it has one.
    found: Option<Manifest>,
}

/// Runs `work`, which puts the manifest `new` in the place of the one the
/// file system of `start` has, once for every operation of it after which a
/// crash can leave something new. The power goes off after that operation.
/// Every state in `plans` that the crash can leave must have the old
/// manifest or the new one, as the step allows, and is handed to `each`.
///
/// Returns the steps the crashes came after.
fn crash_a_replacement(
    start: &dyn Fn() -> SimVfs,
    work: &dyn Fn(&SimVfs) -> Result<(), SegmentStoreError>,
    new: &Manifest,
    plans: &Plans,
    each: &mut dyn FnMut(Crashed<'_>),
) -> Vec<&'static str> {
    let mut steps = Vec::new();
    for point in 0.. {
        let fs = start();
        let old = manifest_in(&fs);
        let begun = fs.stats();
        fs.crash_at(CrashPoint::AfterChange(begun.changes + point));
        let result = work(&fs);
        if !fs.is_powered_off() {
            result.expect("the replacement works with the power on");
            break;
        }
        let last = last_op_before_power_off(&fs).filter(|last| last.op > begun.ops);
        let (step, allowed, returns) = left_after(last.as_ref());
        match &result {
            Ok(()) => assert!(returns, "returned Ok after: {step}"),
            Err(error) => {
                assert!(!returns, "failed after: {step}");
                assert!(is_power_loss(error), "{error}");
            }
        }

        let image = fs.crash_image();
        let crash = crash_states(&image, plans);
        assert!(crash.exhaustive || plans.samples > 0, "after: {step}");
        let mut left = BTreeSet::new();
        for (plan, state) in crash.states {
            let context = || format!("crash after: {step}\n{image:?}\n{plan:?}");
            let _context = OnFailure(&context);
            let found = manifest_in(&state);
            let which = if found == old {
                Left::Old
            } else if found.as_ref() == Some(new) {
                Left::New
            } else {
                panic!("MANIFEST is neither the old one nor the new one: {found:?}")
            };
            assert!(allowed.contains(&which), "{which:?} manifest");
            left.insert(which);
            each(Crashed {
                image: &image,
                plan: &plan,
                state,
                found,
            });
        }
        assert_eq!(left.into_iter().collect::<Vec<_>>(), allowed, "{step}");
        steps.push(step);
    }
    steps
}

fn open_and_replace(fs: &SimVfs, next: &Manifest) -> Result<(), SegmentStoreError> {
    let mut store = ManifestStore::open(shared(fs), db_dir())?;
    replace(&mut store, next.clone())
}

/// A session after the crash finds the manifest the crash left, removes the
/// temporary file, and replaces the manifest by `next`, which is then
/// durable.
fn recovery_completes(crashed: &Crashed<'_>, next: &Manifest) {
    let fs = &crashed.state;
    let mut store = ManifestStore::open(shared(fs), db_dir()).expect("the manifest opens");
    assert_eq!(Some(store.manifest().unwrap()), crashed.found.as_ref());
    assert!(!has_temporary_file(fs));
    replace(&mut store, next.clone()).expect("the manifest is replaced");
    assert!(!has_temporary_file(fs));
    let image = fs.crash_image();
    let durable = image.recover(&image.lose_all()).unwrap();
    assert_eq!(manifest_in(&durable).as_ref(), Some(next));
}

#[test]
fn a_crash_at_any_step_of_a_replacement_leaves_the_old_manifest_or_the_new_one() {
    let first = with_another_log(&first_manifest());
    let second = with_another_log(&first);
    let third = with_another_log(&second);
    for (sector_size, plans, crash_recovery_too) in [
        (512, EVERY_STATE, true),
        (16, EVERY_STATE, false),
        (1, SOME_STATES, false),
    ] {
        let start = || {
            let fs = sim_fs(traced(7, sector_size));
            let mut store = ManifestStore::create(shared(&fs), db_dir(), first_manifest()).unwrap();
            replace(&mut store, first.clone()).unwrap();
            fs
        };
        let mut seen = BTreeSet::new();
        let mut states = Vec::new();
        let steps = crash_a_replacement(
            &start,
            &|fs| open_and_replace(fs, &second),
            &second,
            &plans,
            &mut |crashed| {
                recovery_completes(&crashed, &third);
                if seen.insert(crashed.image.recover(crashed.plan).unwrap().fingerprint()) {
                    states.push((crashed.image.clone(), crashed.plan.clone()));
                }
            },
        );
        assert_eq!(steps, REPLACEMENT);
        println!(
            "sectors of {sector_size}: {} crash points, {} distinct states",
            steps.len(),
            states.len()
        );
        if !crash_recovery_too {
            continue;
        }

        let mut with_stale_file = 0;
        for (image, plan) in &states {
            let start = || image.recover(plan).unwrap();
            let found = manifest_in(&start()).expect("a manifest");
            let next = with_another_log(&found);
            let after = with_another_log(&next);
            let steps = crash_a_replacement(
                &start,
                &|fs| open_and_replace(fs, &next),
                &next,
                &plans,
                &mut |crashed| recovery_completes(&crashed, &after),
            );
            if has_temporary_file(&start()) {
                with_stale_file += 1;
                assert_eq!(steps[1], "the stale temporary file is removed");
                assert_eq!(steps[2..], REPLACEMENT[1..]);
            } else {
                assert_eq!(steps, REPLACEMENT);
            }
        }
        assert!(with_stale_file >= 2, "{with_stale_file}");
    }
}

#[test]
fn a_crash_while_the_first_manifest_is_written_leaves_none_or_a_whole_one() {
    let first = first_manifest();
    for (sector_size, plans) in [(512, EVERY_STATE), (16, EVERY_STATE), (1, SOME_STATES)] {
        let steps = crash_a_replacement(
            &|| sim_fs(traced(3, sector_size)),
            &|fs| ManifestStore::create(shared(fs), db_dir(), first.clone()).map(drop),
            &first,
            &plans,
            &mut |crashed| {
                let fs = &crashed.state;
                if crashed.found.is_none() {
                    let missing = ManifestStore::open(shared(fs), db_dir()).unwrap_err();
                    assert!(matches!(missing, SegmentStoreError::NoManifest { .. }));
                    ManifestStore::create(shared(fs), db_dir(), first.clone())
                        .expect("the first manifest is written after all");
                }
                let exists =
                    ManifestStore::create(shared(fs), db_dir(), first.clone()).unwrap_err();
                assert!(matches!(exists, SegmentStoreError::ManifestExists { .. }));
                recovery_completes(
                    &Crashed {
                        found: Some(first.clone()),
                        ..crashed
                    },
                    &with_another_log(&first),
                );
            },
        );
        assert_eq!(steps, REPLACEMENT);
    }
}

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
    /// States that had the manifest of the last replacement that returned.
    old: usize,
    /// States that had the manifest a replacement was installing.
    new: usize,
}

/// Runs `workload` on a new file system once for every operation of it after
/// which a crash can leave something new. The power goes off after that
/// operation, and every state in `plans` that the crash can leave is checked
/// against the model of the run.
fn crash_everywhere(
    config: &SimConfig,
    seed: u64,
    plans: &Plans,
    workload: impl Fn(&mut Run) -> Result<(), SegmentStoreError>,
) -> Tally {
    let mut tally = Tally::default();
    for point in 0.. {
        let fs = sim_fs(config.clone());
        fs.crash_at(CrashPoint::AfterChange(fs.stats().changes + point));
        let mut run = Run::new(shared(&fs), db_dir(), seed);
        let result = workload(&mut run);
        if !fs.is_powered_off() {
            result.expect("the workload runs with the power on");
            break;
        }
        if let Err(error) = result {
            assert!(is_power_loss(&error), "crash point {point}: {error}");
        }
        check_crash(&fs, &run, plans, &mut tally, &|| {
            format!("seed {seed}, crash point {point}")
        });
    }
    tally
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
        if check_recovered(Arc::new(state), db_dir(), &run.model) {
            tally.new += 1;
        } else {
            tally.old += 1;
        }
    }
}

#[test]
fn every_state_of_a_crash_at_any_step_of_a_segments_life_has_a_manifest_and_its_segments() {
    let tally = crash_everywhere(&SimConfig::new(11), 11, &EVERY_STATE, |run| {
        run.script(&SEGMENT_LIFE)
    });
    println!("{tally:?}");
    assert_eq!(tally.exhaustive, tally.crash_points);
    assert!(tally.crash_points >= 70, "{tally:?}");
    assert!(tally.old > 0 && tally.new > 0, "{tally:?}");
}

#[test]
fn a_crash_tears_writes_at_any_sixteen_bytes_of_a_segments_life() {
    let config = SimConfig {
        sector_size: 16,
        ..SimConfig::new(12)
    };
    let tally = crash_everywhere(&config, 12, &SOME_STATES, |run| run.script(&SEGMENT_LIFE));
    println!("{tally:?}");
    assert!(tally.old > 0 && tally.new > 0, "{tally:?}");
}

#[test]
fn a_crash_at_any_operation_of_a_drawn_workload_leaves_what_the_model_allows() {
    let mut all = Tally::default();
    for seed in [21, 22, 23] {
        let tally = crash_everywhere(&SimConfig::new(seed), seed, &SOME_STATES, |run| run.run(70));
        println!("seed {seed}: {tally:?}");
        assert!(tally.crash_points >= 100, "{tally:?}");
        all.old += tally.old;
        all.new += tally.new;
    }
    assert!(all.old > 0 && all.new > 0, "{all:?}");
}

#[test]
fn a_database_goes_on_through_one_crash_after_another() {
    let mut tally = Tally::default();
    for seed in [31, 32, 33, 34] {
        let mut fs = sim_fs(SimConfig::new(seed));
        let mut run = Run::new(shared(&fs), db_dir(), seed);
        for crash in 0..25 {
            let more = 1 + (seed + 7 * crash) % 23;
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
            let left = image.recover(&image.sample(crash)).unwrap();
            run.crashed(shared(&left), manifest_in(&left));
            fs = left;
        }
        let target = run.steps + 60;
        run.run(target).expect("the workload goes on");
        run.check_session();
    }
    println!("{tally:?}");
    assert_eq!(tally.crash_points, 100);
    assert!(tally.old > 0 && tally.new > 0, "{tally:?}");
}
