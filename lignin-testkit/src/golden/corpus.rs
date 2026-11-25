//! The corpus on disk: recording it, replaying it and dumping it in full.
//!
//! The corpus is a directory of trace files, `v<grove version>/<trace>.trace`
//! for the standard scale and `reference/v<grove version>.trace` for the
//! reference scale. [`record()`] writes them, [`replay`] runs every trace
//! again and reports where it differs from its file, and [`dump`] writes
//! traces with every step in full into a directory of the caller's, for
//! comparing two checkouts with `diff`.

use std::{
    collections::BTreeSet,
    fmt, fs, io,
    ops::Range,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    thread,
};

pub use super::trace::Tally;
use super::{
    plan::{plan, Planned, Scale},
    record::{self, Detailing, Engine},
    run::STACK_BYTES,
    trace::{Detail, Difference, ParseError, Trace},
    Error,
};

/// The crates the recorded results come from: every workspace member that
/// is not part of LigninDB. The newest commit that touched one of them is
/// the engine commit a trace names.
pub const ENGINE_CRATES: &[&str] = &[
    "costs",
    "grovedb",
    "grovedb-bincode",
    "grovedb-bincode-derive",
    "grovedb-bulk-append-tree",
    "grovedb-commitment-tree",
    "grovedb-dense-fixed-sized-merkle-tree",
    "grovedb-element",
    "grovedb-epoch-based-storage-flags",
    "grovedb-merkle-mountain-range",
    "grovedb-private-document-store",
    "grovedb-query",
    "grovedb-version",
    "grovedbg-types",
    "merk",
    "path",
    "storage",
    "visualize",
];

/// The corpus committed with this crate.
pub fn corpus_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("corpus")
}

/// The checkout this crate was built from.
pub fn repository_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

/// Which traces to work on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    /// The scale.
    pub scale: Scale,
    /// Only the traces of this name.
    pub only: Option<String>,
    /// Only the traces of the grove version with this protocol version.
    pub grove_version: Option<u32>,
}

impl Selection {
    /// Every trace of `scale`.
    pub fn all(scale: Scale) -> Self {
        Selection {
            scale,
            only: None,
            grove_version: None,
        }
    }

    fn is_whole(&self) -> bool {
        self.only.is_none() && self.grove_version.is_none()
    }

    /// The selected traces of the plan.
    pub fn traces(&self) -> Vec<Planned> {
        plan(self.scale)
            .into_iter()
            .filter(|planned| {
                self.only
                    .as_ref()
                    .is_none_or(|only| *only == planned.spec.name)
                    && self.grove_version.is_none_or(|version| {
                        version == planned.spec.grove_version.protocol_version
                    })
            })
            .collect()
    }
}

/// How to run traces.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// The directory the databases are created in, each in a directory of
    /// its own that is removed afterwards.
    pub scratch: PathBuf,
    /// How many traces run at once.
    pub jobs: usize,
    /// The engine the traces run on. A replay or a dump runs on any; the
    /// corpus is recorded from RocksDB only.
    pub engine: Engine,
}

impl Default for Settings {
    /// Databases in the system's temporary directory, and as many traces at
    /// once as there are processors, eight at most.
    fn default() -> Self {
        Settings {
            scratch: std::env::temp_dir(),
            jobs: thread::available_parallelism().map_or(1, |jobs| jobs.get().min(8)),
            engine: Engine::RocksDb,
        }
    }
}

/// A trace file that was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Written {
    /// The file, relative to the directory written into.
    pub file: String,
    /// How many steps the trace has.
    pub steps: u64,
    /// The digest of the trace.
    pub digest: String,
    /// The size of the file.
    pub bytes: u64,
    /// How many steps of the trace did not succeed.
    pub tally: Tally,
    /// Whether the trace is a hand-written case.
    pub hand_written: bool,
}

impl Written {
    /// Whether the trace is a generated workload that records an operation
    /// that fails or a proof that does not verify. Every generated stream
    /// is meant to apply and prove on its grove version, so this is a fault
    /// of the generator or of the engine, and not something to commit.
    pub fn is_unexpected(&self) -> bool {
        !self.hand_written && !self.tally.is_clean()
    }
}

/// Records the selected traces into `corpus`, replacing their files.
/// Recording a whole scale also removes the trace files of that scale the
/// plan no longer produces.
pub fn record(
    corpus: &Path,
    selection: &Selection,
    engine: &str,
    settings: &Settings,
) -> Result<Vec<Written>, Error> {
    if settings.engine != Engine::RocksDb {
        return Err(Error::Refused(format!(
            "the corpus is recorded from RocksDB, not from {}",
            settings.engine
        )));
    }
    let written = write(corpus, selection, engine, Detailing::Sampled, settings)?;
    if selection.is_whole() {
        for stray in strays(corpus, selection.scale)? {
            let path = corpus.join(&stray);
            fs::remove_file(&path).map_err(|error| Error::io(&path, &error))?;
        }
    }
    Ok(written)
}

/// Writes the selected traces into `out` with every step in full, or with
/// the steps numbered `steps` in full and the rest as the corpus has them.
///
/// A range is what the reference scale needs: a million steps in full are
/// gigabytes, and a replay that differs there names the thousand steps
/// between two checkpoints.
pub fn dump(
    out: &Path,
    selection: &Selection,
    engine: &str,
    steps: Option<Range<u64>>,
    settings: &Settings,
) -> Result<Vec<Written>, Error> {
    match steps {
        Some(steps) => {
            let steps: BTreeSet<u64> = steps.collect();
            write(out, selection, engine, Detailing::Steps(&steps), settings)
        }
        None => write(out, selection, engine, Detailing::Everything, settings),
    }
}

fn write(
    directory: &Path,
    selection: &Selection,
    engine: &str,
    detailing: Detailing,
    settings: &Settings,
) -> Result<Vec<Written>, Error> {
    let planned = selection.traces();
    in_parallel(&planned, settings.jobs, |planned| {
        let trace = record::record(
            &planned.spec,
            settings.engine,
            engine,
            detailing,
            &settings.scratch,
        )?;
        let text = trace.render();
        let path = directory.join(&planned.file);
        let parent = path.parent().unwrap_or(directory);
        fs::create_dir_all(parent).map_err(|error| Error::io(parent, &error))?;
        fs::write(&path, &text).map_err(|error| Error::io(&path, &error))?;
        Ok(Written {
            file: planned.file.clone(),
            steps: trace.steps,
            digest: trace.digest,
            bytes: text.len() as u64,
            tally: trace.tally,
            hand_written: planned.hand_written,
        })
    })
    .into_iter()
    .collect()
}

/// The trace files of `scale` in `corpus` that the plan does not produce.
fn strays(corpus: &Path, scale: Scale) -> Result<Vec<String>, Error> {
    let planned: BTreeSet<String> = plan(scale)
        .into_iter()
        .map(|planned| planned.file)
        .collect();
    let names = |directory: &Path| -> Result<Vec<String>, Error> {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(Error::io(directory, &error)),
        };
        entries
            .map(|entry| match entry {
                Ok(entry) => Ok(entry.file_name().to_string_lossy().into_owned()),
                Err(error) => Err(Error::io(directory, &error)),
            })
            .collect()
    };
    let is_of_scale = |directory: &str| match scale {
        Scale::Standard => directory
            .strip_prefix('v')
            .is_some_and(|version| version.parse::<u32>().is_ok()),
        Scale::Reference => directory == "reference",
    };
    let mut strays = Vec::new();
    for directory in names(corpus)? {
        if !is_of_scale(&directory) {
            continue;
        }
        for name in names(&corpus.join(&directory))? {
            let file = format!("{directory}/{name}");
            if name.ends_with(".trace") && !planned.contains(&file) {
                strays.push(file);
            }
        }
    }
    strays.sort();
    Ok(strays)
}

/// What replaying the corpus found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Report {
    /// The engine the traces were replayed on.
    pub engine: Engine,
    /// Every selected trace.
    pub traces: Vec<Replayed>,
    /// The trace files the plan does not produce. Only looked for when a
    /// whole scale is replayed.
    pub strays: Vec<String>,
}

/// One replayed trace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Replayed {
    /// The file of the trace, relative to the corpus directory.
    pub file: String,
    /// The name of the trace.
    pub name: String,
    /// Its seed.
    pub seed: u64,
    /// The protocol version of its grove version.
    pub grove_version: u32,
    /// What the replay found.
    pub outcome: Outcome,
}

/// What replaying one trace found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every step is as recorded.
    Same {
        /// How many steps were compared.
        steps: u64,
    },
    /// The trace has no file.
    Missing,
    /// The file is not a trace.
    Unreadable(ParseError),
    /// The replay differs from the file.
    Differs {
        /// The engine commit the file was recorded from.
        engine: String,
        /// Every difference, in the order of the trace.
        differences: Vec<Difference>,
        /// The first differing step as the replay has it, in full. Not
        /// given for a checkpointed trace, which would have to run again.
        replayed: Option<Detail>,
    },
}

impl Report {
    /// Whether every trace is as recorded and no file is left over.
    pub fn is_green(&self) -> bool {
        self.strays.is_empty()
            && self
                .traces
                .iter()
                .all(|trace| matches!(trace.outcome, Outcome::Same { .. }))
    }

    /// How many steps were compared and found the same.
    pub fn steps(&self) -> u64 {
        self.traces
            .iter()
            .map(|trace| match trace.outcome {
                Outcome::Same { steps } => steps,
                _ => 0,
            })
            .sum()
    }
}

/// How many differences of one trace a report lists.
const DIFFERENCES_SHOWN: usize = 5;

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let same = self
            .traces
            .iter()
            .filter(|trace| matches!(trace.outcome, Outcome::Same { .. }))
            .count();
        writeln!(
            f,
            "golden corpus: {} traces replayed on {}, {same} as recorded ({} steps), {} not",
            self.traces.len(),
            self.engine,
            self.steps(),
            self.traces.len() - same + self.strays.len(),
        )?;
        for trace in &self.traces {
            let heading = format!(
                "{}: {}, seed {:#x}, grove version {}",
                trace.file, trace.name, trace.seed, trace.grove_version
            );
            match &trace.outcome {
                Outcome::Same { .. } => {}
                Outcome::Missing => {
                    writeln!(f, "{heading}: not recorded. Record the corpus again.")?;
                }
                Outcome::Unreadable(error) => writeln!(f, "{heading}: unreadable: {error}")?,
                Outcome::Differs {
                    engine,
                    differences,
                    replayed,
                } => {
                    writeln!(
                        f,
                        "{heading}: {} differences from the recording of engine commit {engine}",
                        differences.len()
                    )?;
                    for difference in differences.iter().take(DIFFERENCES_SHOWN) {
                        writeln!(f, "  {difference}")?;
                    }
                    if differences.len() > DIFFERENCES_SHOWN {
                        writeln!(f, "  … and {} more", differences.len() - DIFFERENCES_SHOWN)?;
                    }
                    if let Some(detail) = replayed {
                        writeln!(f, "  step {} as replayed:", detail.step)?;
                        for remark in &detail.remarks {
                            writeln!(f, "    # {remark}")?;
                        }
                        for line in &detail.lines {
                            writeln!(f, "    {line}")?;
                        }
                    }
                    writeln!(
                        f,
                        "  the steps in full, to compare with a dump of that commit: \
                         lignin-golden dump --only {} --grove-version {}{}{} --out <directory>",
                        trace.name,
                        trace.grove_version,
                        match self.engine {
                            Engine::RocksDb => String::new(),
                            #[allow(unreachable_patterns)]
                            other => format!(" --engine {}", other.command_name()),
                        },
                        if trace.file.starts_with("reference/") {
                            " --reference --steps <first>..<last>"
                        } else {
                            ""
                        },
                    )?;
                }
            }
        }
        for stray in &self.strays {
            writeln!(
                f,
                "{stray}: recorded, but no longer part of the corpus. Record the corpus again."
            )?;
        }
        Ok(())
    }
}

/// Runs the selected traces again and compares each with its file in
/// `corpus`.
pub fn replay(corpus: &Path, selection: &Selection, settings: &Settings) -> Result<Report, Error> {
    let planned = selection.traces();
    let traces = in_parallel(&planned, settings.jobs, |planned| {
        Ok(Replayed {
            file: planned.file.clone(),
            name: planned.spec.name.clone(),
            seed: planned.spec.seed,
            grove_version: planned.spec.grove_version.protocol_version,
            outcome: replay_one(corpus, planned, settings)?,
        })
    })
    .into_iter()
    .collect::<Result<Vec<Replayed>, Error>>()?;
    let strays = if selection.is_whole() {
        strays(corpus, selection.scale)?
    } else {
        Vec::new()
    };
    Ok(Report {
        engine: settings.engine,
        traces,
        strays,
    })
}

fn replay_one(corpus: &Path, planned: &Planned, settings: &Settings) -> Result<Outcome, Error> {
    let path = corpus.join(&planned.file);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Outcome::Missing),
        Err(error) => return Err(Error::io(&path, &error)),
    };
    let recorded = match Trace::parse(&text) {
        Ok(recorded) => recorded,
        Err(error) => return Ok(Outcome::Unreadable(error)),
    };
    let in_full: BTreeSet<u64> = recorded.details().map(|detail| detail.step).collect();
    let run = |steps: &BTreeSet<u64>| {
        record::record(
            &planned.spec,
            settings.engine,
            &recorded.engine,
            Detailing::Steps(steps),
            &settings.scratch,
        )
    };
    let replayed = run(&in_full)?;
    let differences = replayed.differences_from(&recorded);
    if differences.is_empty() {
        return Ok(Outcome::Same {
            steps: replayed.steps,
        });
    }
    let checkpointed = planned
        .spec
        .segments
        .iter()
        .any(|segment| segment.checkpoint_every.is_some());
    let first = differences.iter().find_map(|difference| difference.step);
    let first_in_full = match first {
        Some(step) if !checkpointed => match replayed.detail(step) {
            Some(detail) => Some(detail.clone()),
            None => run(&BTreeSet::from([step]))?.detail(step).cloned(),
        },
        _ => None,
    };
    Ok(Outcome::Differs {
        engine: recorded.engine,
        differences,
        replayed: first_in_full,
    })
}

/// Works on every item, `jobs` at a time, each on a thread with the stack a
/// trace needs. The results are in the order of the items.
pub(crate) fn in_parallel<T: Sync, R: Send>(
    items: &[T],
    jobs: usize,
    work: impl Fn(&T) -> R + Sync,
) -> Vec<R> {
    let next = AtomicUsize::new(0);
    let results: Vec<Mutex<Option<R>>> = items.iter().map(|_| Mutex::new(None)).collect();
    thread::scope(|scope| {
        for _ in 0..jobs.clamp(1, items.len().max(1)) {
            thread::Builder::new()
                .stack_size(STACK_BYTES)
                .spawn_scoped(scope, || loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else {
                        break;
                    };
                    let result = work(item);
                    *results[index]
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(result);
                })
                .expect("a thread can be started");
        }
    });
    results
        .into_iter()
        .map(|result| {
            result
                .into_inner()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .expect("every item was worked on")
        })
        .collect()
}

/// The commit of the engine in the checkout at `repository`: the newest
/// commit that touched one of the [`ENGINE_CRATES`].
///
/// It is an error for one of them to have uncommitted changes: a recording
/// has to name the code it came from.
pub fn engine_commit(repository: &Path) -> Result<String, Error> {
    let git = |arguments: &[&str]| -> Result<String, Error> {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .arg("--")
            .args(ENGINE_CRATES)
            .output()
            .map_err(|error| Error::Git(format!("git could not be run: {error}")))?;
        if !output.status.success() {
            return Err(Error::Git(format!(
                "git {} failed in {}: {}",
                arguments[0],
                repository.display(),
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    };
    let changed = git(&["status", "--porcelain"])?;
    if !changed.is_empty() {
        return Err(Error::Git(format!(
            "the engine crates have uncommitted changes, so no commit names what would be \
             recorded:\n{changed}"
        )));
    }
    let commit = git(&["log", "-1", "--format=%H"])?;
    if commit.is_empty() {
        return Err(Error::Git(format!(
            "no commit in {} touches the engine crates",
            repository.display()
        )));
    }
    Ok(commit)
}

/// A git repository to take an engine commit from, for tests that must not
/// depend on the state of the checkout they run in.
#[cfg(test)]
pub(crate) mod test_repository {
    use std::{fs, path::Path, process::Command};

    pub(crate) fn git(repository: &Path, arguments: &[&str]) -> String {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args([
                "-c",
                "user.name=golden",
                "-c",
                "user.email=golden@example.invalid",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(arguments)
            .output()
            .expect("git runs");
        assert!(output.status.success(), "git {arguments:?}: {output:?}");
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    /// A repository whose one commit touches an engine crate, and that
    /// commit.
    pub(crate) fn with_an_engine_commit() -> (tempfile::TempDir, String) {
        let repository = tempfile::tempdir().expect("temp dir");
        let at = repository.path();
        git(at, &["init", "--quiet"]);
        fs::create_dir(at.join("merk")).expect("create");
        fs::write(at.join("merk/lib.rs"), "one").expect("write");
        git(at, &["add", "."]);
        git(at, &["commit", "--quiet", "-m", "merk"]);
        let commit = git(at, &["rev-parse", "HEAD"]);
        (repository, commit)
    }
}

#[cfg(test)]
mod tests {
    use grovedb_version::version::GROVE_VERSIONS;

    use super::*;
    use crate::golden::{plan::SEED, trace::Entry};

    fn settings() -> Settings {
        Settings {
            jobs: 4,
            ..Settings::default()
        }
    }

    /// The smallest whole selection there is: one workload's trace for one
    /// grove version.
    fn one(name: &str, grove_version: u32) -> Selection {
        Selection {
            scale: Scale::Standard,
            only: Some(name.to_string()),
            grove_version: Some(grove_version),
        }
    }

    const CASE: &str = "legacy-reference-in-provable-count-sum-tree";

    #[test]
    fn a_selection_filters_the_plan() {
        let all = Selection::all(Scale::Standard).traces();
        assert_eq!(all.len(), plan(Scale::Standard).len());
        let documents = Selection {
            only: Some("documents".to_string()),
            ..Selection::all(Scale::Standard)
        }
        .traces();
        assert_eq!(documents.len(), GROVE_VERSIONS.len());
        assert!(documents
            .iter()
            .all(|planned| planned.spec.name == "documents"));
        let third = Selection {
            grove_version: Some(3),
            ..Selection::all(Scale::Standard)
        }
        .traces();
        assert_eq!(third.len(), all.len() / GROVE_VERSIONS.len());
        let both = one("documents", 3).traces();
        assert_eq!(both.len(), 1);
        assert_eq!(both[0].file, "v3/documents.trace");
        assert_eq!(both[0].spec.seed, SEED);
        assert!(one("no-such-trace", 3).traces().is_empty());
        assert_eq!(
            Selection::all(Scale::Reference).traces().len(),
            GROVE_VERSIONS.len()
        );
    }

    #[test]
    fn the_default_settings_use_the_temporary_directory() {
        let settings = Settings::default();
        assert_eq!(settings.scratch, std::env::temp_dir());
        assert!((1..=8).contains(&settings.jobs));
        assert!(corpus_dir().ends_with("lignin-testkit/corpus"));
        assert!(repository_dir().join("lignin-testkit").is_dir());
    }

    #[test]
    fn a_recorded_trace_replays_the_same() {
        let corpus = tempfile::tempdir().expect("temp dir");
        let selection = Selection {
            only: Some(CASE.to_string()),
            ..Selection::all(Scale::Standard)
        };
        let written = record(corpus.path(), &selection, "engine", &settings()).expect("record");
        assert_eq!(written.len(), GROVE_VERSIONS.len());
        for (written, grove_version) in written.iter().zip(GROVE_VERSIONS) {
            let file = format!("v{}/{CASE}.trace", grove_version.protocol_version);
            assert_eq!(written.file, file);
            let text = fs::read_to_string(corpus.path().join(&file)).expect("read");
            assert_eq!(written.bytes, text.len() as u64);
            let trace = Trace::parse(&text).expect("parse");
            assert_eq!(
                (trace.steps, &trace.digest),
                (written.steps, &written.digest)
            );
            assert_eq!(trace.engine, "engine");
            assert_eq!(trace.details().count() as u64, trace.steps);
            assert_eq!(written.tally, trace.tally);
            assert!(written.hand_written && !written.is_unexpected());
            assert_eq!(
                written.tally.is_clean(),
                grove_version.protocol_version > 2,
                "the case is one grove versions 1 and 2 cannot verify"
            );
        }
        let report = replay(corpus.path(), &selection, &settings()).expect("replay");
        assert!(report.is_green(), "{report}");
        assert_eq!(report.traces.len(), GROVE_VERSIONS.len());
        assert_eq!(
            report.steps(),
            written.iter().map(|written| written.steps).sum::<u64>()
        );
        assert_eq!(
            report.to_string(),
            format!(
                "golden corpus: 5 traces replayed on {}, 5 as recorded ({} steps), 0 not\n",
                Engine::RocksDb,
                report.steps()
            )
        );
    }

    #[test]
    fn a_replay_reports_what_differs_and_where() {
        let corpus = tempfile::tempdir().expect("temp dir");
        let selection = one("balances", 5);
        record(corpus.path(), &selection, "abc123", &settings()).expect("record");
        let path = corpus.path().join("v5/balances.trace");
        let recorded = Trace::parse(&fs::read_to_string(&path).expect("read")).expect("parse");

        let mut cost_changed = recorded.clone();
        let Entry::Step(line) = &mut cost_changed.segments[0].entries[0] else {
            panic!("a trace starts with a step");
        };
        let was = line.clone();
        line.replace_range(1..5, if &was[1..5] == "0000" { "ffff" } else { "0000" });
        fs::write(&path, cost_changed.render()).expect("write");
        let report = replay(corpus.path(), &selection, &settings()).expect("replay");
        assert!(!report.is_green());
        assert_eq!(report.steps(), 0);
        let Outcome::Differs {
            engine,
            differences,
            replayed,
        } = &report.traces[0].outcome
        else {
            panic!("the replay differs: {report}");
        };
        assert_eq!(engine, "abc123");
        assert_eq!(differences.len(), 1, "{report}");
        assert_eq!(differences[0].step, Some(0));
        assert!(
            differences[0].what.starts_with("balances: cost differ: "),
            "{report}"
        );
        let replayed = replayed.as_ref().expect("the first differing step in full");
        assert_eq!(replayed.step, 0);
        assert!(replayed.lines[0].starts_with("cost seeks="));
        let shown = report.to_string();
        let heading = format!(
            "golden corpus: 1 traces replayed on {}, 0 as recorded (0 steps), 1 not\n",
            Engine::RocksDb
        );
        for expected in [
            heading.as_str(),
            "v5/balances.trace: balances, seed 0x1, grove version 5: 1 differences from the \
             recording of engine commit abc123\n",
            "  step 0: balances: cost differ: ",
            "  step 0 as replayed:\n    # Write(Insert",
            "    cost seeks=",
            "lignin-golden dump --only balances --grove-version 5 --out <directory>",
            "  the steps in full, to compare with a dump of that commit: ",
        ] {
            assert!(shown.contains(expected), "{shown}\nlacks {expected:?}");
        }

        #[cfg(grovedb_engine_seam)]
        {
            let on_lignindb = Settings {
                engine: Engine::LigninDb,
                ..settings()
            };
            let report = replay(corpus.path(), &selection, &on_lignindb).expect("replay");
            let Outcome::Differs { differences, .. } = &report.traces[0].outcome else {
                panic!("the replay on LigninDB differs: {report}");
            };
            assert_eq!(differences.len(), 1, "{report}");
            assert_eq!(differences[0].step, Some(0));
            let shown = report.to_string();
            for expected in [
                "golden corpus: 1 traces replayed on LigninDB, 0 as recorded (0 steps), 1 not\n",
                "  step 0: balances: cost differ: ",
                "lignin-golden dump --only balances --grove-version 5 --engine lignindb --out \
                 <directory>",
            ] {
                assert!(shown.contains(expected), "{shown}\nlacks {expected:?}");
            }
            assert_eq!(
                record(corpus.path(), &selection, "abc123", &on_lignindb),
                Err(Error::Refused(
                    "the corpus is recorded from RocksDB, not from LigninDB".to_string()
                ))
            );
        }

        let unsampled = recorded.segments[0]
            .entries
            .iter()
            .enumerate()
            .filter(|(_, entry)| matches!(entry, Entry::Step(_)))
            .map(|(position, _)| position)
            .nth(40)
            .expect("the trace has forty steps");
        let mut many = recorded.clone();
        for entry in &mut many.segments[0].entries[unsampled..] {
            if let Entry::Step(line) = entry {
                *line = format!("c{}", "f".repeat(4));
            }
        }
        fs::write(&path, many.render()).expect("write");
        let report = replay(corpus.path(), &selection, &settings()).expect("replay");
        let Outcome::Differs {
            differences,
            replayed,
            ..
        } = &report.traces[0].outcome
        else {
            panic!("the replay differs: {report}");
        };
        assert!(differences.len() > DIFFERENCES_SHOWN);
        assert_eq!(differences[0].step, Some(40));
        assert_eq!(
            replayed.as_ref().map(|detail| detail.step),
            Some(40),
            "a step the recording does not have in full is run again for the report"
        );
        assert!(report.to_string().contains(&format!(
            "  … and {} more\n",
            differences.len() - DIFFERENCES_SHOWN
        )));

        let mut other_stream = recorded.clone();
        other_stream.segments[0].stream = "0".repeat(64);
        fs::write(&path, other_stream.render()).expect("write");
        let report = replay(corpus.path(), &selection, &settings()).expect("replay");
        let Outcome::Differs {
            differences,
            replayed,
            ..
        } = &report.traces[0].outcome
        else {
            panic!("the replay differs: {report}");
        };
        assert!(differences[0]
            .what
            .contains("operation stream of balances changed"));
        assert_eq!(*replayed, None, "no step differs, so none is shown");

        fs::write(&path, "not a trace\n").expect("write");
        let report = replay(corpus.path(), &selection, &settings()).expect("replay");
        assert!(matches!(report.traces[0].outcome, Outcome::Unreadable(_)));
        assert!(
            report.to_string().contains("unreadable: line 1: expected"),
            "{report}"
        );

        fs::remove_file(&path).expect("remove");
        let report = replay(corpus.path(), &selection, &settings()).expect("replay");
        assert_eq!(report.traces[0].outcome, Outcome::Missing);
        assert!(!report.is_green());
        assert!(report
            .to_string()
            .contains("not recorded. Record the corpus again."));

        fs::create_dir(&path).expect("a directory in place of the file");
        let error = replay(corpus.path(), &selection, &settings()).expect_err("unreadable");
        assert!(
            matches!(&error, Error::Io { path: at, .. } if *at == path),
            "{error}"
        );
    }

    #[test]
    fn left_over_files_are_found_by_a_whole_replay_and_removed_by_a_whole_recording() {
        let corpus = tempfile::tempdir().expect("temp dir");
        assert_eq!(strays(corpus.path(), Scale::Standard), Ok(Vec::new()));
        for directory in ["v1", "v5", "reference", "v9"] {
            fs::create_dir(corpus.path().join(directory)).expect("create");
        }
        for file in [
            "v1/gone.trace",
            "v5/documents.trace",
            "v5/notes.txt",
            "v5/also-gone.trace",
            "reference/v1.trace",
            "reference/v9.trace",
            "v9/unknown.trace",
        ] {
            fs::write(corpus.path().join(file), "x").expect("write");
        }
        assert_eq!(
            strays(corpus.path(), Scale::Standard),
            Ok(vec![
                "v1/gone.trace".to_string(),
                "v5/also-gone.trace".to_string(),
                "v9/unknown.trace".to_string(),
            ]),
            "a grove version the plan does not know is looked into as well"
        );
        assert_eq!(
            strays(corpus.path(), Scale::Reference),
            Ok(vec!["reference/v9.trace".to_string()])
        );
        let report = Report {
            engine: Engine::RocksDb,
            traces: Vec::new(),
            strays: strays(corpus.path(), Scale::Reference).expect("strays"),
        };
        assert!(!report.is_green());
        assert_eq!(
            report.to_string(),
            format!(
                "golden corpus: 0 traces replayed on {}, 0 as recorded (0 steps), 1 not\n\
                 reference/v9.trace: recorded, but no longer part of the corpus. Record the \
                 corpus again.\n",
                Engine::RocksDb
            )
        );
        let filtered =
            replay(corpus.path(), &one("no-such-trace", 5), &settings()).expect("replay");
        assert!(
            filtered.is_green(),
            "a filtered replay does not look for left-over files"
        );

        fs::write(corpus.path().join("v2"), "a file in place of a directory").expect("write");
        let error = strays(corpus.path(), Scale::Standard).expect_err("unreadable");
        assert!(matches!(error, Error::Io { .. }), "{error}");
    }

    #[test]
    fn a_dump_has_every_step_in_full_and_the_same_digest() {
        let corpus = tempfile::tempdir().expect("temp dir");
        let out = tempfile::tempdir().expect("temp dir");
        let selection = one("balances", 1);
        let recorded = record(corpus.path(), &selection, "engine", &settings()).expect("record");
        let dumped = dump(out.path(), &selection, "engine", None, &settings()).expect("dump");
        assert_eq!(dumped[0].digest, recorded[0].digest);
        assert!(dumped[0].bytes > recorded[0].bytes);
        let text = fs::read_to_string(out.path().join("v1/balances.trace")).expect("read");
        let trace = Trace::parse(&text).expect("parse");
        assert_eq!(trace.details().count() as u64, trace.steps);

        let some = dump(out.path(), &selection, "engine", Some(30..33), &settings()).expect("dump");
        assert_eq!(some[0].digest, recorded[0].digest);
        let text = fs::read_to_string(out.path().join("v1/balances.trace")).expect("read");
        let trace = Trace::parse(&text).expect("parse");
        let in_full: Vec<u64> = trace.details().map(|detail| detail.step).collect();
        assert_eq!(in_full, [30, 31, 32]);

        let unwritable = out.path().join("v1/balances.trace");
        let error = dump(&unwritable, &selection, "engine", None, &settings()).expect_err("a file");
        assert!(matches!(error, Error::Io { .. }), "{error}");
    }

    #[test]
    fn work_is_done_in_order_whatever_the_number_of_jobs() {
        let items: Vec<u32> = (0..50).collect();
        for jobs in [0, 1, 3, 200] {
            assert_eq!(
                in_parallel(&items, jobs, |item| item * 2),
                items.iter().map(|item| item * 2).collect::<Vec<_>>()
            );
        }
        assert_eq!(
            in_parallel(&[] as &[u32], 4, |item| *item),
            Vec::<u32>::new()
        );
    }

    #[test]
    fn the_engine_commit_is_the_newest_one_touching_an_engine_crate() {
        use test_repository::git;

        let repository = tempfile::tempdir().expect("temp dir");
        let at = repository.path();
        let not_a_repository = engine_commit(at).expect_err("no repository");
        assert!(
            not_a_repository.to_string().contains("git status failed"),
            "{not_a_repository}"
        );

        git(at, &["init", "--quiet"]);
        fs::create_dir(at.join("lignin-testkit")).expect("create");
        fs::write(at.join("lignin-testkit/lib.rs"), "one").expect("write");
        git(at, &["add", "."]);
        git(at, &["commit", "--quiet", "-m", "testkit"]);
        let untouched = engine_commit(at).expect_err("no engine commit");
        assert!(
            untouched.to_string().contains("no commit in"),
            "{untouched}"
        );

        fs::create_dir(at.join("merk")).expect("create");
        fs::write(at.join("merk/lib.rs"), "one").expect("write");
        let uncommitted = engine_commit(at).expect_err("uncommitted");
        assert!(
            uncommitted.to_string().contains("uncommitted changes"),
            "{uncommitted}"
        );
        assert!(uncommitted.to_string().contains("merk/"), "{uncommitted}");

        git(at, &["add", "."]);
        git(at, &["commit", "--quiet", "-m", "merk"]);
        let engine = engine_commit(at).expect("engine commit");
        assert_eq!(engine, git(at, &["rev-parse", "HEAD"]));

        fs::write(at.join("lignin-testkit/lib.rs"), "two").expect("write");
        git(at, &["add", "."]);
        git(at, &["commit", "--quiet", "-m", "testkit again"]);
        assert_eq!(
            engine_commit(at),
            Ok(engine.clone()),
            "a LigninDB commit is not an engine commit"
        );

        let (fixture, commit) = test_repository::with_an_engine_commit();
        assert_eq!(engine_commit(fixture.path()), Ok(commit));
    }

    /// The list a recording's commit is taken from has every workspace
    /// member that is not a LigninDB crate.
    #[test]
    fn the_engine_crates_are_the_workspace_members_outside_lignindb() {
        let manifest = fs::read_to_string(repository_dir().join("Cargo.toml")).expect("read");
        let members = manifest
            .split_once("members = [")
            .and_then(|(_, rest)| rest.split_once(']'))
            .expect("the workspace lists its members")
            .0;
        let mut engine: Vec<&str> = members
            .split(',')
            .map(|member| member.trim().trim_matches('"'))
            .filter(|member| !member.is_empty() && !member.starts_with("lignin"))
            .collect();
        engine.sort_unstable();
        assert_eq!(engine, ENGINE_CRATES);
    }
}
