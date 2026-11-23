//! The fuzzing loop: case after case on two engines, until they differ or
//! the time is up.
//!
//! The cases of a run are numbered, and the seed of case `n` is a function
//! of the run's seed and `n`. Workers take the numbers in turn, each with a
//! pair of fresh databases for every case, so a run with more workers
//! covers the same cases sooner and a divergence is reproduced from the
//! seed of its case alone ([`Case::generate`]).
//!
//! A run ends at the divergence of the case with the lowest number. When a
//! case diverges, the cases with higher numbers are abandoned and the ones
//! with lower numbers run to their end, since one of them may diverge too.
//! So a run of a given number of cases reports the same case whatever the
//! number of workers. The stream is then shrunk ([`super::shrink`]).

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
    families::Family,
    pair::{catching, run, Divergence, Side, Stored, Tally},
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
    /// The cases run to their end, or to the step both engines failed at.
    pub cases: u64,
    /// What was compared in the cases that have ended, the ones that were
    /// abandoned included.
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
    /// The stream shrunk, when the run was asked to shrink.
    pub shrunk: Option<Shrunk>,
    /// Why the stream was not shrunk although the run was asked to: what
    /// stopped the shrinking.
    pub not_shrunk: Option<String>,
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
    /// The cases that ended at a step of a dropped tree that is counted
    /// apart ([`Family::DroppedTreeProof`], [`Family::DroppedTreeAssertion`]):
    /// which of the two, the seed of the case and the step, in the order
    /// of the seeds.
    pub apart: Vec<(Family, u64, usize)>,
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

/// Counts a worker out when it ends, however it ends: a worker that
/// panicked would otherwise be waited for.
struct Working<'a>(&'a AtomicUsize);

impl Drop for Working<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
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
    // Set when the time is up or a worker failed: every case is abandoned.
    let halt = AtomicBool::new(false);
    // The lowest number of a case that diverged. The cases above it are
    // abandoned, and no case is started any more.
    let lowest = AtomicU64::new(u64::MAX);
    let next = AtomicU64::new(0);
    let cases = AtomicU64::new(0);
    let working = AtomicUsize::new(settings.jobs.max(1));
    let tally = Mutex::new(Tally::default());
    let found = Mutex::new(None::<(u64, Case, Divergence, Duration)>);
    let failure = Mutex::new(None);
    let first_cut_short = Mutex::new(None::<(u64, String)>);
    let apart = Mutex::new(Vec::new());

    let work = || -> Result<(), Error> {
        loop {
            let index = next.fetch_add(1, Ordering::Relaxed);
            let abandoned =
                || halt.load(Ordering::Relaxed) || lowest.load(Ordering::Relaxed) < index;
            if abandoned() || settings.cases.is_some_and(|cases| index >= cases) {
                return Ok(());
            }
            let case = Case::generate(case_seed(settings.seed, index), &settings.shape);
            let run = run(
                &case,
                reference,
                candidate,
                scratch.path(),
                settings.stored,
                &abandoned,
            )?;
            tally.lock().expect("no worker panics").add(&run.tally);
            if let (
                Some(family @ (Family::DroppedTreeProof | Family::DroppedTreeAssertion)),
                Some(ended),
            ) = (run.ended_at, &run.ended_with)
            {
                let mut apart = apart.lock().expect("no worker panics");
                apart.push((family, case.seed, ended.step));
            }
            if run.interrupted {
                return Ok(());
            }
            if let Some(divergence) = run.divergence {
                lowest.fetch_min(index, Ordering::Relaxed);
                let mut found = found.lock().expect("no worker panics");
                if found.as_ref().is_none_or(|(earlier, ..)| index < *earlier) {
                    *found = Some((index, case, divergence, started.elapsed()));
                }
                return Ok(());
            }
            if let Some(why) = run.cut_short {
                let mut first = first_cut_short.lock().expect("no worker panics");
                if first.as_ref().is_none_or(|(earlier, _)| index < *earlier) {
                    *first = Some((index, why));
                }
            }
            cases.fetch_add(1, Ordering::Relaxed);
        }
    };
    let worker = || {
        let _working = Working(&working);
        let failed = match catching(work) {
            Ok(Ok(())) => return,
            Ok(Err(error)) => error,
            Err(panic) => Error::engine("running a case", &format!("it panicked: {panic}")),
        };
        halt.store(true, Ordering::Relaxed);
        failure
            .lock()
            .expect("no worker panics")
            .get_or_insert(failed);
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
                halt.store(true, Ordering::Relaxed);
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

    let finding =
        found
            .into_inner()
            .expect("no worker panics")
            .map(|(index, case, divergence, after)| {
                // A stream that cannot be shrunk is still the finding.
                let (shrunk, not_shrunk) = match settings.shrink {
                    None => (None, None),
                    Some(budget) => {
                        let shrunk = on_a_deep_stack(|| {
                            catching(|| {
                                shrink(
                                    &case,
                                    &divergence,
                                    reference,
                                    candidate,
                                    scratch.path(),
                                    budget,
                                )
                            })
                        });
                        match shrunk {
                            Ok(Ok(shrunk)) => (Some(shrunk), None),
                            Ok(Err(error)) => (None, Some(error.to_string())),
                            Err(panic) => (None, Some(format!("the shrinking panicked: {panic}"))),
                        }
                    }
                };
                Finding {
                    index,
                    case,
                    divergence,
                    after,
                    shrunk,
                    not_shrunk,
                }
            });
    let mut apart = apart.into_inner().expect("no worker panics");
    apart.sort_unstable_by_key(|(_, seed, _)| *seed);
    Ok(Summary {
        progress,
        finding,
        first_cut_short: first_cut_short
            .into_inner()
            .expect("no worker panics")
            .map(|(_, why)| why),
        apart,
    })
}

#[cfg(test)]
#[expect(
    clippy::result_large_err,
    reason = "a database is opened with GroveDB's own error, whatever its size"
)]
mod tests {
    use super::*;
    use crate::diff::{
        engines::{Fault, StandIn},
        pair::{
            testing::{rocksdb, scratch, stand_in, stand_in_that_opens, unhurried},
            Part,
        },
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

    fn fuzzed<B: StreamEngine>(candidate: &Side<B>, settings: &Settings) -> Summary {
        fuzz(&rocksdb(), candidate, settings, &mut |_| {}).expect("the run")
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
        let scratch = scratch();
        let mut reports = 0;
        let summary = fuzz(
            &rocksdb(),
            &stand_in(),
            &Settings {
                cases: Some(12),
                jobs: 3,
                progress_every: Duration::ZERO,
                ..settings(scratch.path())
            },
            &mut |progress| {
                assert!(progress.cases <= 12);
                reports += 1;
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
        assert_eq!(summary.progress.tally.cut_short, 0);
        assert_eq!(summary.first_cut_short, None);
        assert!(reports > 0);
        assert_eq!(
            std::fs::read_dir(scratch.path())
                .expect("the directory")
                .count(),
            0,
            "the databases are gone"
        );

        // The same cases whatever the number of workers.
        let alone = fuzzed(
            &stand_in(),
            &Settings {
                cases: Some(12),
                jobs: 1,
                ..settings(scratch.path())
            },
        );
        assert_eq!(alone.progress.tally, summary.progress.tally);
    }

    #[test]
    fn a_run_ends_when_its_time_is_up() {
        let scratch = scratch();
        let duration = Duration::from_millis(1500);
        let summary = fuzzed(
            &stand_in(),
            &Settings {
                duration: Some(duration),
                jobs: 2,
                ..settings(scratch.path())
            },
        );
        assert!(summary.finding.is_none());
        assert!(summary.progress.elapsed >= duration);
        // A case abandoned at the end is no case that was cut short.
        assert_eq!(summary.progress.tally.cut_short, 0);
        assert_eq!(summary.first_cut_short, None);
    }

    /// The fault is one most cases meet, at steps of their own, so with
    /// several workers a case with a higher number diverges first. The
    /// one with the lowest number is reported all the same.
    #[test]
    fn a_run_ends_at_the_lowest_case_the_engines_differ_on_and_shrinks_it() {
        let scratch = scratch();
        let candidate = StandIn::side(Fault::AddedByte { when_added: 0 });
        let run = |jobs, shrink| {
            fuzzed(
                &candidate,
                &Settings {
                    cases: Some(400),
                    jobs,
                    shrink,
                    ..settings(scratch.path())
                },
            )
        };
        let summary = run(1, Some(unhurried()));
        let finding = summary.finding.expect("the planted fault");
        assert_eq!(finding.divergence.parts, [Part::Cost]);
        assert_eq!(finding.case.seed, case_seed(1, finding.index));
        assert!(finding.after <= summary.progress.elapsed);
        assert_eq!(finding.not_shrunk, None);
        let shrunk = finding.shrunk.expect("the stream is shrunk");
        assert!(shrunk.reproduced && shrunk.minimal);
        assert!(shrunk.case.ops.len() <= finding.divergence.step + 1);
        assert!(shrunk.case.ops.len() < 12, "{:?}", shrunk.case.ops);
        assert_eq!(shrunk.divergence.parts, [Part::Cost]);

        // The case is the same one from its seed alone, and with more
        // workers it is the one reported.
        let again = Case::generate(finding.case.seed, &settings(scratch.path()).shape);
        assert_eq!(again.ops, finding.case.ops);
        for jobs in [4, 8] {
            let several = run(jobs, None).finding.expect("the planted fault");
            assert!(several.shrunk.is_none() && several.not_shrunk.is_none());
            assert_eq!(several.index, finding.index);
            assert_eq!(several.divergence, finding.divergence);
        }
    }

    #[test]
    fn a_case_both_engines_panic_on_is_cut_short_and_the_run_goes_on() {
        let scratch = scratch();
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
        assert!(
            why.starts_with("at step ") && why.contains(", both engines panicked: "),
            "{why}"
        );
    }

    #[test]
    fn a_run_that_cannot_open_a_database_fails() {
        let scratch = scratch();
        let closed = Side::<StandIn>::opened_by("closed", |_| {
            Err(grovedb::Error::InvalidInput("no database here"))
        });
        let few = Settings {
            cases: Some(8),
            ..settings(scratch.path())
        };
        let refused =
            fuzz(&rocksdb(), &closed, &few, &mut |_| {}).expect_err("the candidate does not open");
        assert!(refused.to_string().contains("opening a database of closed"));
        let nowhere = fuzz(
            &rocksdb(),
            &stand_in(),
            &Settings {
                cases: Some(1),
                ..settings(&scratch.path().join("missing"))
            },
            &mut |_| {},
        )
        .expect_err("there is no scratch directory");
        assert!(matches!(nowhere, Error::Io { .. }));
    }

    /// A worker that panics where no engine is asked anything ends the run
    /// with what it said. It is not waited for.
    #[test]
    fn a_worker_that_panics_fails_the_run() {
        let scratch = scratch();
        // No grove version to draw a case for.
        let no_version = Settings {
            cases: Some(8),
            jobs: 3,
            shape: Shape {
                grove_versions: Vec::new(),
                ..Shape::default()
            },
            ..settings(scratch.path())
        };
        let failed = fuzz(&rocksdb(), &stand_in(), &no_version, &mut |_| {})
            .expect_err("no case can be generated")
            .to_string();
        assert!(
            failed.starts_with("the engine failed running a case: it panicked: "),
            "{failed}"
        );
    }

    /// A stream that cannot be shrunk is reported as it was found.
    #[test]
    fn a_finding_is_kept_when_its_stream_cannot_be_shrunk() {
        let scratch = scratch();
        let fault = Fault::AddedByte { when_added: 0 };
        let one_worker = Settings {
            cases: Some(400),
            jobs: 1,
            ..settings(scratch.path())
        };
        let found = fuzzed(&StandIn::side(fault), &one_worker)
            .finding
            .expect("the planted fault");
        // One worker opens one candidate for each case up to the one that
        // diverges. An engine that opens no more often cannot be shrunk on.
        let worn_out = stand_in_that_opens(found.index + 1, fault);
        let finding = fuzzed(&worn_out, &one_worker)
            .finding
            .expect("the planted fault");
        assert_eq!(finding.index, found.index);
        assert_eq!(finding.divergence, found.divergence);
        assert!(finding.shrunk.is_none());
        let why = finding.not_shrunk.expect("the shrinking failed");
        assert!(why.ends_with("no database any more"), "{why}");
    }

    #[test]
    fn the_bytes_under_a_directory_are_those_of_its_files() {
        let dir = scratch();
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
