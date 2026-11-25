//! The `lignin-golden` command: record, replay, dump and list the corpus.

use std::{
    io::Write,
    ops::Range,
    path::{Path, PathBuf},
};

use super::{
    corpus::{self, corpus_dir, engine_commit, repository_dir, Selection, Settings, Written},
    plan::Scale,
    record::Engine,
    Error,
};

/// How the command is used.
pub const USAGE: &str = "\
usage: lignin-golden <command> [options]

commands:
  record    run every trace and write the corpus
  replay    run every trace and compare it with the corpus
  dump      write traces with every step in full, to compare two checkouts
  list      list the traces of the corpus

options:
  --reference         the million-document reference scale in place of the
                      standard one (minutes and a gigabyte of scratch per
                      grove version; one trace at a time unless --jobs is given)
  --only NAME         only the traces of this name
  --grove-version N   only the traces of this grove version
  --corpus DIR        the corpus directory (default: lignin-testkit/corpus)
  --scratch DIR       where databases are created (default: the temporary
                      directory)
  --jobs N            how many traces run at once (default: one per
                      processor, eight at most)
  --engine NAME       replay and dump: the engine to run on, rocksdb (the
                      default) or lignindb; the corpus is recorded from
                      rocksdb
  --out DIR           dump: the directory to write into
  --steps FROM..TO    dump: only these steps in full, the rest as the corpus
                      has them (the way to look into the reference scale)

A recording names the engine commit it came from: the newest commit that
touched an engine crate of this checkout. record refuses to run while an
engine crate has uncommitted changes. A dump of such a checkout says so.
";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Record,
    Replay,
    Dump,
    List,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Invocation {
    command: Command,
    selection: Selection,
    corpus: PathBuf,
    settings: Settings,
    out: Option<PathBuf>,
    steps: Option<Range<u64>>,
}

fn parse(arguments: &[String]) -> Result<Invocation, String> {
    let mut arguments = arguments.iter();
    let command = match arguments.next().map(String::as_str) {
        Some("record") => Command::Record,
        Some("replay") => Command::Replay,
        Some("dump") => Command::Dump,
        Some("list") => Command::List,
        Some(other) => return Err(format!("`{other}` is not a command")),
        None => return Err("no command given".to_string()),
    };
    let mut invocation = Invocation {
        command,
        selection: Selection::all(Scale::Standard),
        corpus: corpus_dir(),
        settings: Settings::default(),
        out: None,
        steps: None,
    };
    let mut jobs = None;
    while let Some(option) = arguments.next() {
        let mut value = || {
            arguments
                .next()
                .cloned()
                .ok_or_else(|| format!("`{option}` needs a value"))
        };
        let number = |value: String| {
            value
                .parse::<u32>()
                .map_err(|_| format!("`{value}` is not a number"))
        };
        match option.as_str() {
            "--reference" => invocation.selection.scale = Scale::Reference,
            "--only" => invocation.selection.only = Some(value()?),
            "--grove-version" => invocation.selection.grove_version = Some(number(value()?)?),
            "--corpus" => invocation.corpus = PathBuf::from(value()?),
            "--scratch" => invocation.settings.scratch = PathBuf::from(value()?),
            "--jobs" => jobs = Some(number(value()?)?.max(1) as usize),
            "--engine" => invocation.settings.engine = Engine::named(&value()?)?,
            "--out" => invocation.out = Some(PathBuf::from(value()?)),
            "--steps" => {
                let value = value()?;
                let range = value.split_once("..").and_then(|(from, to)| {
                    Some(from.parse::<u64>().ok()?..to.parse::<u64>().ok()?)
                });
                invocation.steps =
                    Some(range.ok_or_else(|| format!("`{value}` is not a range FROM..TO"))?);
            }
            other => return Err(format!("`{other}` is not an option")),
        }
    }
    invocation.settings.jobs = match (jobs, invocation.selection.scale) {
        (Some(jobs), _) => jobs,
        (None, Scale::Reference) => 1,
        (None, Scale::Standard) => invocation.settings.jobs,
    };
    if command == Command::Dump && invocation.out.is_none() {
        return Err("dump needs `--out DIR`".to_string());
    }
    if command != Command::Dump && (invocation.out.is_some() || invocation.steps.is_some()) {
        return Err("`--out` and `--steps` are options of dump".to_string());
    }
    if command == Command::Record && invocation.settings.engine != Engine::RocksDb {
        return Err("the corpus is recorded from rocksdb only".to_string());
    }
    Ok(invocation)
}

/// Runs the command and returns its exit code: 0 for success, 1 for a
/// replay that differs from the corpus or a generated workload that records
/// a failure, 2 for a command that could not run.
pub fn run(arguments: &[String], out: &mut dyn Write) -> u8 {
    run_in(&repository_dir(), arguments, out)
}

/// [`run`], taking the engine commit from the checkout at `repository`.
fn run_in(repository: &Path, arguments: &[String], out: &mut dyn Write) -> u8 {
    let invocation = match parse(arguments) {
        Ok(invocation) => invocation,
        Err(error) => {
            let _ = writeln!(out, "error: {error}\n\n{USAGE}");
            return 2;
        }
    };
    match execute(&invocation, repository, out) {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(out, "error: {error}");
            2
        }
    }
}

fn execute(invocation: &Invocation, repository: &Path, out: &mut dyn Write) -> Result<u8, Error> {
    let Invocation {
        selection,
        corpus,
        settings,
        ..
    } = invocation;
    let traces = selection.traces();
    if traces.is_empty() {
        let _ = writeln!(out, "error: no trace of the corpus is selected");
        return Ok(2);
    }
    match invocation.command {
        Command::List => {
            for planned in traces {
                let workloads: Vec<&str> = planned
                    .spec
                    .segments
                    .iter()
                    .map(|segment| segment.workload.as_str())
                    .collect();
                let _ = writeln!(
                    out,
                    "{}: {}, seed {:#x}, grove version {}, workloads {}",
                    planned.file,
                    planned.spec.name,
                    planned.spec.seed,
                    planned.spec.grove_version.protocol_version,
                    workloads.join(" ")
                );
            }
            Ok(0)
        }
        Command::Record => {
            let engine = engine_commit(repository)?;
            let written = corpus::record(corpus, selection, &engine, settings)?;
            Ok(summarize(out, "recorded", &written, &engine, corpus))
        }
        Command::Dump => {
            let engine = engine_commit(repository).unwrap_or_else(|_| "uncommitted".to_string());
            let directory = invocation.out.as_deref().unwrap_or(corpus);
            let steps = invocation.steps.clone();
            let written = corpus::dump(directory, selection, &engine, steps, settings)?;
            Ok(summarize(out, "dumped", &written, &engine, directory))
        }
        Command::Replay => {
            let report = corpus::replay(corpus, selection, settings)?;
            let _ = write!(out, "{report}");
            Ok(u8::from(!report.is_green()))
        }
    }
}

/// Says what was written, and returns the exit code: 1 when a generated
/// workload records an operation that fails or a proof that does not
/// verify.
fn summarize(
    out: &mut dyn Write,
    done: &str,
    written: &[Written],
    engine: &str,
    directory: &Path,
) -> u8 {
    let steps: u64 = written.iter().map(|written| written.steps).sum();
    let bytes: u64 = written.iter().map(|written| written.bytes).sum();
    let _ = writeln!(
        out,
        "{done} {} traces ({steps} steps, {bytes} bytes) from engine commit {engine} into {}",
        written.len(),
        directory.display()
    );
    let mut code = 0;
    for written in written.iter().filter(|written| written.is_unexpected()) {
        let _ = writeln!(
            out,
            "{}: a generated workload records {}. Every generated stream is meant to apply and \
             prove, so this is not a trace to commit.",
            written.file, written.tally
        );
        code = 1;
    }
    code
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    fn output(line: &str) -> (u8, String) {
        let mut out = Vec::new();
        let code = run(&arguments(line), &mut out);
        (
            code,
            String::from_utf8(out).expect("the command writes text"),
        )
    }

    /// [`output`], with the engine commit taken from `repository`.
    fn output_in(repository: &Path, line: &str) -> (u8, String) {
        let mut out = Vec::new();
        let code = run_in(repository, &arguments(line), &mut out);
        (
            code,
            String::from_utf8(out).expect("the command writes text"),
        )
    }

    #[test]
    fn options_are_read() {
        let invocation = parse(&arguments(
            "dump --reference --only reference --grove-version 4 --corpus /c --scratch /s --jobs \
             3 --out /o --steps 2000..3000",
        ))
        .expect("parse");
        assert_eq!(
            invocation,
            Invocation {
                command: Command::Dump,
                selection: Selection {
                    scale: Scale::Reference,
                    only: Some("reference".to_string()),
                    grove_version: Some(4),
                },
                corpus: PathBuf::from("/c"),
                settings: Settings {
                    scratch: PathBuf::from("/s"),
                    jobs: 3,
                    engine: Engine::RocksDb,
                },
                out: Some(PathBuf::from("/o")),
                steps: Some(2000..3000),
            }
        );
        let defaults = parse(&arguments("replay")).expect("parse");
        assert_eq!(defaults.command, Command::Replay);
        assert_eq!(defaults.selection, Selection::all(Scale::Standard));
        assert_eq!(defaults.corpus, corpus_dir());
        assert_eq!(defaults.settings, Settings::default());
        assert_eq!(
            parse(&arguments("record")).expect("parse").command,
            Command::Record
        );
        assert_eq!(
            parse(&arguments("list")).expect("parse").command,
            Command::List
        );
        assert_eq!(
            parse(&arguments("replay --reference"))
                .expect("parse")
                .settings
                .jobs,
            1,
            "the reference scale runs one trace at a time unless told otherwise"
        );
        assert_eq!(
            parse(&arguments("replay --jobs 0"))
                .expect("parse")
                .settings
                .jobs,
            1
        );
    }

    #[test]
    fn what_is_not_a_command_line_is_refused_with_the_usage() {
        for (line, message) in [
            ("", "no command given"),
            ("rerecord", "`rerecord` is not a command"),
            ("replay --fast", "`--fast` is not an option"),
            ("replay --only", "`--only` needs a value"),
            ("replay --jobs many", "`many` is not a number"),
            ("replay --grove-version v5", "`v5` is not a number"),
            ("dump", "dump needs `--out DIR`"),
            (
                "replay --out /o",
                "`--out` and `--steps` are options of dump",
            ),
            (
                "record --steps 1..2",
                "`--out` and `--steps` are options of dump",
            ),
            ("dump --out /o --steps 5", "`5` is not a range FROM..TO"),
            (
                "dump --out /o --steps a..9",
                "`a..9` is not a range FROM..TO",
            ),
            (
                "replay --engine abc",
                "`abc` is not an engine: rocksdb or lignindb",
            ),
            ("replay --engine", "`--engine` needs a value"),
            (
                "record --engine lignindb",
                if cfg!(grovedb_engine_seam) {
                    "the corpus is recorded from rocksdb only"
                } else {
                    "this GroveDB has no LigninDB engine"
                },
            ),
        ] {
            let (code, text) = output(line);
            assert_eq!(code, 2, "{line}");
            assert!(
                text.starts_with(&format!("error: {message}\n\nusage:")),
                "{text}"
            );
        }
    }

    #[test]
    fn list_names_every_selected_trace() {
        let (code, text) = output("list --grove-version 2 --only suite");
        assert_eq!(code, 0);
        assert!(
            text.starts_with(
                "v2/suite.trace: suite, seed 0x4c49474e494e, grove version 2, workloads documents \
                 balances tokens"
            ),
            "{text}"
        );
        assert_eq!(text.lines().count(), 1);
        let (code, text) = output("list --reference --grove-version 5");
        assert_eq!(code, 0);
        assert_eq!(
            text,
            "reference/v5.trace: reference, seed 0x1, grove version 5, workloads \
             documents-reference named-proofs\n"
        );
        let (code, text) = output("list --only no-such-trace");
        assert_eq!(
            (code, text.as_str()),
            (2, "error: no trace of the corpus is selected\n")
        );
    }

    #[test]
    fn a_trace_is_recorded_replayed_and_dumped() {
        let corpus = tempfile::tempdir().expect("temp dir");
        let dumped = tempfile::tempdir().expect("temp dir");
        let (repository, engine) = corpus::test_repository::with_an_engine_commit();
        let output = |line: &str| output_in(repository.path(), line);
        let scope = "--only legacy-query-in-provable-count-indexed-tree --grove-version 2";
        let at = corpus.path().display();

        let (code, text) = output(&format!("record {scope} --corpus {at}"));
        assert_eq!(code, 0, "{text}");
        assert!(text.starts_with("recorded 1 traces (10 steps, "), "{text}");
        assert!(
            text.contains(&format!(" from engine commit {engine} into ")),
            "{text}"
        );

        let (code, text) = output(&format!("replay {scope} --corpus {at}"));
        assert_eq!(code, 0, "{text}");
        assert_eq!(
            text,
            format!(
                "golden corpus: 1 traces replayed on {}, 1 as recorded (10 steps), 0 not\n",
                Engine::RocksDb
            )
        );
        #[cfg(grovedb_engine_seam)]
        {
            let (code, text) = output(&format!("replay {scope} --corpus {at} --engine lignindb"));
            assert_eq!(
                (code, text.as_str()),
                (
                    0,
                    "golden corpus: 1 traces replayed on LigninDB, 1 as recorded (10 steps), 0 \
                     not\n"
                )
            );
        }

        let (code, text) = output(&format!("dump {scope} --out {}", dumped.path().display()));
        assert_eq!(code, 0, "{text}");
        assert!(text.starts_with("dumped 1 traces (10 steps, "), "{text}");
        let file = "v2/legacy-query-in-provable-count-indexed-tree.trace";
        assert_eq!(
            std::fs::read_to_string(dumped.path().join(file)).expect("the dump"),
            std::fs::read_to_string(corpus.path().join(file)).expect("the recording"),
            "a hand-written case is recorded in full, so its dump is its recording"
        );

        let (code, text) = output(&format!(
            "dump {scope} --out {} --steps 2..4",
            dumped.path().display()
        ));
        assert_eq!(code, 0, "{text}");
        let some = std::fs::read_to_string(dumped.path().join(file)).expect("the dump");
        assert_eq!(some.matches("\n  step ").count(), 2, "{some}");

        std::fs::write(corpus.path().join(file), "not a trace").expect("write");
        let (code, text) = output(&format!("replay {scope} --corpus {at}"));
        assert_eq!(code, 1, "{text}");
        assert!(text.contains("unreadable"), "{text}");

        let (code, text) = output(&format!("record {scope} --corpus {at}"));
        assert_eq!(code, 0, "{text}");
        let (code, text) = output(&format!(
            "replay {scope} --corpus {at} --scratch /nonexistent"
        ));
        assert_eq!(code, 2, "{text}");
        assert!(text.starts_with("error: /nonexistent: "), "{text}");
    }

    /// A recording has to name the code it came from, so it is refused
    /// while an engine crate has uncommitted changes. A dump is for looking
    /// at exactly such a checkout, and says what it is of.
    #[test]
    fn a_checkout_with_uncommitted_engine_changes_is_dumped_but_not_recorded() {
        let corpus = tempfile::tempdir().expect("temp dir");
        let (repository, _) = corpus::test_repository::with_an_engine_commit();
        std::fs::write(repository.path().join("merk/lib.rs"), "changed").expect("write");
        let scope = "--only legacy-query-in-provable-count-indexed-tree --grove-version 5";
        let at = corpus.path().display();

        let (code, text) = output_in(repository.path(), &format!("record {scope} --corpus {at}"));
        assert_eq!(code, 2, "{text}");
        assert!(
            text.starts_with("error: the engine crates have uncommitted changes"),
            "{text}"
        );
        assert!(
            !corpus.path().join("v5").exists(),
            "nothing is written when a recording is refused"
        );

        let (code, text) = output_in(repository.path(), &format!("dump {scope} --out {at}"));
        assert_eq!(code, 0, "{text}");
        assert!(
            text.contains(" from engine commit uncommitted into "),
            "{text}"
        );
    }

    #[test]
    fn a_generated_workload_that_records_a_failure_is_pointed_out() {
        let written = |file: &str, hand_written: bool, unverified: u64| Written {
            file: file.to_string(),
            steps: 10,
            digest: "0".repeat(64),
            bytes: 100,
            tally: corpus::Tally {
                unverified,
                ..corpus::Tally::default()
            },
            hand_written,
        };
        let mut out = Vec::new();
        let expected = [
            written("v1/documents.trace", false, 0),
            written("v1/legacy-case.trace", true, 2),
        ];
        assert_eq!(
            summarize(&mut out, "recorded", &expected, "abc", "/c".as_ref()),
            0
        );
        assert_eq!(
            String::from_utf8(out).expect("text"),
            "recorded 2 traces (20 steps, 200 bytes) from engine commit abc into /c\n"
        );
        let mut out = Vec::new();
        let unexpected = [written("v1/documents.trace", false, 3)];
        assert_eq!(
            summarize(&mut out, "recorded", &unexpected, "abc", "/c".as_ref()),
            1
        );
        let text = String::from_utf8(out).expect("text");
        assert!(
            text.contains(
                "\nv1/documents.trace: a generated workload records failed=0 unproved=0 \
                 unverified=3. "
            ),
            "{text}"
        );
    }

    /// The command takes the engine commit from the checkout the crate was
    /// built from, whatever state that checkout is in.
    #[test]
    fn the_engine_commit_defaults_to_the_checkout() {
        let corpus = tempfile::tempdir().expect("temp dir");
        let scope = "--only legacy-query-in-provable-count-indexed-tree --grove-version 5";
        let expected = engine_commit(&repository_dir());
        let (code, text) = output(&format!(
            "record {scope} --corpus {}",
            corpus.path().display()
        ));
        let (dump_code, dump_text) =
            output(&format!("dump {scope} --out {}", corpus.path().display()));
        assert_eq!(dump_code, 0, "{dump_text}");
        match expected {
            Ok(commit) => {
                assert_eq!(code, 0, "{text}");
                assert!(
                    text.contains(&format!("from engine commit {commit} into")),
                    "{text}"
                );
                assert!(dump_text.contains(&format!("from engine commit {commit} into")));
            }
            Err(error) => {
                assert_eq!(code, 2, "{text}");
                assert_eq!(text, format!("error: {error}\n"));
                assert!(
                    dump_text.contains("from engine commit uncommitted into"),
                    "{dump_text}"
                );
            }
        }
    }
}
