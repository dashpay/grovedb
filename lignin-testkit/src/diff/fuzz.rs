//! The fuzzing loop: case after case on two engines, until they differ or
//! the time is up.
//!
//! The cases of a run are numbered, and the seed of case `n` is a function
//! of the run's seed and `n`. Workers take the numbers in turn, each with a
//! pair of fresh databases for every case, so a run with more workers
//! covers the same cases sooner and a divergence is reproduced from the
//! seed of its case alone ([`Case::generate`]).
//!
//! A run ends at the first divergence. The stream is then shrunk
//! ([`super::shrink`]).

use std::{
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
        Mutex,
    },
    time::{Duration, Instant},
};

use super::{
    case::{Case, Shape},
    pair::{run, Divergence, Side, Stored, Tally},
    shrink::{shrink, Budget, Shrunk},
};
use crate::{
    golden::{record::on_a_deep_stack, run::STACK_BYTES, Error},
    workload::{Rng, StreamEngine},
};

const FUZZ: u64 = 0x6675_7a7a;

/// How often the stored records are compared in a fuzzing run, in
/// operations that are not reads. A walk reads everything a database
/// holds, so a case of a few thousand operations spends about as long
/// walking as applying.
pub const STORED_EVERY: u64 = 32;

/// The seed of case `index` of the run with `seed`.
pub fn case_seed(seed: u64, index: u64) -> u64 {
    Rng::derive(seed, FUZZ, index).next_u64()
}

/// A fuzzing run.
#[derive(Clone, Debug)]
pub struct Settings {
    /// The seed of the run.
    pub seed: u64,
    /// How many cases run at once.
    pub jobs: usize,
    /// Where the databases are created.
    pub scratch: PathBuf,
    /// How long the run lasts. A case under way when the time is up is
    /// abandoned.
    pub duration: Option<Duration>,
    /// How many cases the run has. Without a duration and without a number
    /// of cases it runs until the engines differ.
    pub cases: Option<u64>,
    /// How cases are generated.
    pub shape: Shape,
    /// How often the stored records are compared.
    pub stored: Stored,
    /// How much work shrinking a diverging stream may take. None leaves
    /// the stream as it is.
    pub shrink: Option<Budget>,
    /// How often the run reports its progress.
    pub progress_every: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            seed: 1,
            jobs: std::thread::available_parallelism().map_or(1, |cores| cores.get().min(4)),
            scratch: std::env::temp_dir(),
            duration: None,
            cases: None,
            shape: Shape::default(),
            stored: Stored::Every(STORED_EVERY),
            shrink: Some(Budget::default()),
            progress_every: Duration::from_secs(60),
        }
    }
}

/// Where a run stands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// How long it has run.
    pub elapsed: Duration,
    /// The cases run to their end.
    pub cases: u64,
    /// What was compared in the cases that have ended, the ones that
    /// were cut short or abandoned included.
    pub tally: Tally,
    /// The most bytes the databases of the run held on disk at once, as
    /// far as it was looked at.
    pub scratch_bytes: u64,
}

/// The case two engines answered differently.
#[derive(Clone, Debug)]
pub struct Finding {
    /// The number of the case in the run.
    pub index: u64,
    /// The case.
    pub case: Case,
    /// The first step the engines differ at.
    pub divergence: Divergence,
    /// How long the run had lasted when the divergence showed.
    pub after: Duration,
    /// The stream shrunk, and how long that took, when the run was asked
    /// to shrink.
    pub shrunk: Option<(Shrunk, Duration)>,
}

/// What a run came to.
#[derive(Clone, Debug)]
pub struct Summary {
    /// Where the run stood when it ended.
    pub progress: Progress,
    /// The divergence that ended it, if one did.
    pub finding: Option<Finding>,
    /// Why the first case that ended early without a divergence did: both
    /// engines panicked, or neither could be observed.
    pub first_cut_short: Option<String>,
}

/// The bytes of the files under `dir`. A file that goes while it is
/// looked at counts for nothing: the databases of a run come and go.
fn bytes_under(dir: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.metadata() {
            Ok(metadata) if metadata.is_dir() => bytes_under(&entry.path()),
            Ok(metadata) => metadata.len(),
            Err(_) => 0,
        })
        .sum()
}

/// Runs cases on `reference` and `candidate` as `settings` say, and hands
/// the progress of the run to `report` as it goes.
pub fn fuzz<A: StreamEngine, B: StreamEngine>(
    reference: &Side<A>,
    candidate: &Side<B>,
    settings: &Settings,
    report: &mut dyn FnMut(&Progress),
) -> Result<Summary, Error> {
    // The databases of this run, in a directory of its own, so that what
    // they hold on disk can be measured.
    let scratch = tempfile::Builder::new()
        .prefix("lignin-diff-run-")
        .tempdir_in(&settings.scratch)
        .map_err(|error| Error::io(&settings.scratch, &error))?;
    let started = Instant::now();
    let stop = AtomicBool::new(false);
    let next = AtomicU64::new(0);
    let cases = AtomicU64::new(0);
    let working = AtomicUsize::new(settings.jobs.max(1));
    let tally = Mutex::new(Tally::default());
    let findings = Mutex::new(Vec::new());
    let failure = Mutex::new(None);
    let first_cut_short = Mutex::new(None::<(u64, String)>);

    let worker = || {
        let work = || -> Result<(), Error> {
            loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let over = settings.cases.is_some_and(|cases| index >= cases);
                if over || stop.load(Ordering::Relaxed) {
                    return Ok(());
                }
                let case = Case::generate(case_seed(settings.seed, index), &settings.shape);
                let run = run(
                    &case,
                    reference,
                    candidate,
                    scratch.path(),
                    settings.stored,
                    &|| stop.load(Ordering::Relaxed),
                )?;
                tally.lock().expect("no worker panics").add(&run.tally);
                if let Some(divergence) = run.divergence {
                    stop.store(true, Ordering::Relaxed);
                    findings.lock().expect("no worker panics").push((
                        index,
                        case,
                        divergence,
                        started.elapsed(),
                    ));
                    return Ok(());
                }
                match run.cut_short {
                    // Interrupted: the run is over.
                    Some(_) if stop.load(Ordering::Relaxed) => return Ok(()),
                    Some(why) => {
                        let mut first = first_cut_short.lock().expect("no worker panics");
                        if first.as_ref().is_none_or(|(earlier, _)| index < *earlier) {
                            *first = Some((index, why));
                        }
                    }
                    None => {}
                }
                cases.fetch_add(1, Ordering::Relaxed);
            }
        };
        if let Err(error) = work() {
            stop.store(true, Ordering::Relaxed);
            failure
                .lock()
                .expect("no worker panics")
                .get_or_insert(error);
        }
        working.fetch_sub(1, Ordering::Relaxed);
    };

    let mut scratch_bytes = 0;
    let progress = |scratch_bytes: u64| Progress {
        elapsed: started.elapsed(),
        cases: cases.load(Ordering::Relaxed),
        tally: *tally.lock().expect("no worker panics"),
        scratch_bytes,
    };
    std::thread::scope(|scope| {
        for _ in 0..settings.jobs.max(1) {
            std::thread::Builder::new()
                .stack_size(STACK_BYTES)
                .spawn_scoped(scope, worker)
                .expect("a thread can be started");
        }
        let mut reported = Instant::now();
        let mut measured = Instant::now();
        while working.load(Ordering::Relaxed) > 0 {
            std::thread::sleep(Duration::from_millis(20));
            if settings
                .duration
                .is_some_and(|duration| started.elapsed() >= duration)
            {
                stop.store(true, Ordering::Relaxed);
            }
            if measured.elapsed() >= Duration::from_secs(1) {
                scratch_bytes = scratch_bytes.max(bytes_under(scratch.path()));
                measured = Instant::now();
            }
            if reported.elapsed() >= settings.progress_every {
                report(&progress(scratch_bytes));
                reported = Instant::now();
            }
        }
    });
    if let Some(error) = failure.into_inner().expect("no worker panics") {
        return Err(error);
    }
    let progress = progress(scratch_bytes);

    // Several workers can find a divergence before they see the others
    // stop. The case with the lowest number is the one a run with one
    // worker would have found.
    let mut findings = findings.into_inner().expect("no worker panics");
    findings.sort_by_key(|(index, ..)| *index);
    let finding = match findings.into_iter().next() {
        None => None,
        Some((index, case, divergence, after)) => {
            let shrunk = match settings.shrink {
                None => None,
                Some(budget) => {
                    let shrinking = Instant::now();
                    let shrunk = on_a_deep_stack(|| {
                        shrink(
                            &case,
                            &divergence,
                            reference,
                            candidate,
                            scratch.path(),
                            budget,
                        )
                    })?;
                    Some((shrunk, shrinking.elapsed()))
                }
            };
            Some(Finding {
                index,
                case,
                divergence,
                after,
                shrunk,
            })
        }
    };
    Ok(Summary {
        progress,
        finding,
        first_cut_short: first_cut_short
            .into_inner()
            .expect("no worker panics")
            .map(|(_, why)| why),
    })
}

#[cfg(test)]
#[expect(
    clippy::result_large_err,
    reason = "a database is opened with GroveDB's own error, whatever its size"
)]
mod tests {
    use grovedb::RocksDbEngine;

    use super::*;
    use crate::diff::{
        engines::{Fault, StandIn},
        pair::Part,
    };

    fn settings(scratch: &Path) -> Settings {
        Settings {
            scratch: scratch.to_path_buf(),
            shape: Shape {
                max_ops_per_stream: 120,
                ..Shape::default()
            },
            ..Settings::default()
        }
    }

    #[test]
    fn the_seeds_of_the_cases_are_a_function_of_the_run() {
        assert_eq!(case_seed(1, 0), case_seed(1, 0));
        let seeds: std::collections::BTreeSet<u64> = (0..100)
            .flat_map(|index| [case_seed(1, index), case_seed(2, index)])
            .collect();
        assert_eq!(seeds.len(), 200);
    }

    #[test]
    fn a_run_of_so_many_cases_compares_them_all_and_ends_clean() {
        let scratch = tempfile::tempdir().expect("a directory");
        let reports = AtomicU64::new(0);
        let summary = fuzz(
            &Side::<RocksDbEngine>::of("rocksdb"),
            &Side::<StandIn>::of("stand-in"),
            &Settings {
                cases: Some(12),
                jobs: 3,
                progress_every: Duration::ZERO,
                ..settings(scratch.path())
            },
            &mut |progress| {
                assert!(progress.cases <= 12);
                reports.fetch_add(1, Ordering::Relaxed);
            },
        )
        .expect("the run");
        assert!(summary.finding.is_none(), "{:?}", summary.finding);
        assert_eq!(summary.progress.cases, 12);
        assert!(summary.progress.tally.steps > 12 * 50);
        assert!(summary.progress.tally.walks >= 12);
        assert!(summary.progress.tally.proofs > 0);
        assert!(
            summary.progress.tally.failed > 0,
            "a mutated stream fails somewhere"
        );
        assert!(reports.load(Ordering::Relaxed) > 0);
        assert_eq!(
            std::fs::read_dir(scratch.path())
                .expect("the directory")
                .count(),
            0,
            "the databases are gone"
        );

        // The same cases whatever the number of workers.
        let alone = fuzz(
            &Side::<RocksDbEngine>::of("rocksdb"),
            &Side::<StandIn>::of("stand-in"),
            &Settings {
                cases: Some(12),
                jobs: 1,
                ..settings(scratch.path())
            },
            &mut |_| {},
        )
        .expect("the run");
        assert_eq!(alone.progress.tally, summary.progress.tally);
    }

    #[test]
    fn a_run_ends_when_its_time_is_up() {
        let scratch = tempfile::tempdir().expect("a directory");
        let started = Instant::now();
        let summary = fuzz(
            &Side::<RocksDbEngine>::of("rocksdb"),
            &Side::<StandIn>::of("stand-in"),
            &Settings {
                duration: Some(Duration::from_millis(1500)),
                jobs: 2,
                ..settings(scratch.path())
            },
            &mut |_| {},
        )
        .expect("the run");
        assert!(summary.finding.is_none());
        assert!(summary.progress.tally.steps > 0);
        assert!(started.elapsed() < Duration::from_secs(60));
        assert!(summary.progress.elapsed >= Duration::from_millis(1500));
    }

    #[test]
    fn a_run_ends_at_the_first_case_the_engines_differ_on_and_shrinks_it() {
        let scratch = tempfile::tempdir().expect("a directory");
        // Every batch that adds no byte: a fault most cases meet.
        let fault = Fault::AddedByte { when_added: 0 };
        let run = |jobs, shrink| {
            fuzz(
                &Side::<RocksDbEngine>::of("rocksdb"),
                &StandIn::side(fault),
                &Settings {
                    cases: Some(400),
                    jobs,
                    shrink,
                    ..settings(scratch.path())
                },
                &mut |_| {},
            )
            .expect("the run")
        };
        let summary = run(1, Some(Budget::default()));
        let finding = summary.finding.expect("the planted fault");
        assert_eq!(finding.divergence.parts, [Part::Cost]);
        assert_eq!(finding.case.seed, case_seed(1, finding.index));
        assert!(finding.after <= summary.progress.elapsed);
        let (shrunk, _) = finding.shrunk.expect("the stream is shrunk");
        assert!(shrunk.reproduced && shrunk.minimal);
        assert!(shrunk.case.ops.len() <= finding.divergence.step + 1);
        assert!(shrunk.case.ops.len() < 12, "{:?}", shrunk.case.ops);
        assert_eq!(shrunk.divergence.parts, [Part::Cost]);

        // The case is the same one from its seed alone, and with more
        // workers the lowest of the cases that differ is reported.
        let again = Case::generate(finding.case.seed, &settings(scratch.path()).shape);
        assert_eq!(again.ops, finding.case.ops);
        let several = run(4, None).finding.expect("the planted fault");
        assert!(several.shrunk.is_none());
        assert_eq!(several.index, finding.index);
        assert_eq!(several.divergence, finding.divergence);
    }

    #[test]
    fn a_case_both_engines_panic_on_is_cut_short_and_the_run_goes_on() {
        let scratch = tempfile::tempdir().expect("a directory");
        let fault = Fault::Panic { when_added: 0 };
        let summary = fuzz(
            &StandIn::side(fault),
            &StandIn::side(fault),
            &Settings {
                cases: Some(30),
                jobs: 2,
                ..settings(scratch.path())
            },
            &mut |_| {},
        )
        .expect("the run");
        assert!(summary.finding.is_none());
        assert_eq!(summary.progress.cases, 30);
        assert!(summary.progress.tally.cut_short > 0);
        let why = summary.first_cut_short.expect("a case was cut short");
        assert!(why.starts_with("both engines panicked at step "), "{why}");
    }

    #[test]
    fn a_run_that_cannot_open_a_database_fails() {
        let scratch = tempfile::tempdir().expect("a directory");
        let closed = Side::<StandIn>::opened_by("closed", |_| {
            Err(grovedb::Error::InvalidInput("no database here"))
        });
        let refused = fuzz(
            &Side::<RocksDbEngine>::of("rocksdb"),
            &closed,
            &Settings {
                cases: Some(8),
                ..settings(scratch.path())
            },
            &mut |_| {},
        )
        .expect_err("the candidate does not open");
        assert!(refused.to_string().contains("opening a database of closed"));
        let nowhere = fuzz(
            &Side::<RocksDbEngine>::of("rocksdb"),
            &Side::<StandIn>::of("stand-in"),
            &Settings {
                cases: Some(1),
                ..settings(&scratch.path().join("missing"))
            },
            &mut |_| {},
        )
        .expect_err("there is no scratch directory");
        assert!(matches!(nowhere, Error::Io { .. }));
    }

    #[test]
    fn the_bytes_under_a_directory_are_those_of_its_files() {
        let dir = tempfile::tempdir().expect("a directory");
        assert_eq!(bytes_under(dir.path()), 0);
        std::fs::write(dir.path().join("a"), [0; 10]).expect("a file");
        std::fs::create_dir(dir.path().join("inner")).expect("a directory");
        std::fs::write(dir.path().join("inner/b"), [0; 5]).expect("a file");
        assert_eq!(bytes_under(dir.path()), 15);
        assert_eq!(bytes_under(&dir.path().join("missing")), 0);
    }

    #[test]
    fn the_default_settings_are_a_run_until_the_engines_differ() {
        let settings = Settings::default();
        assert_eq!(settings.duration, None);
        assert_eq!(settings.cases, None);
        assert!((1..=4).contains(&settings.jobs));
        assert_eq!(settings.stored, Stored::Every(STORED_EVERY));
        assert_eq!(settings.shrink, Some(Budget::default()));
    }
}
