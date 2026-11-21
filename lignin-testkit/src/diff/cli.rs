//! The `lignin-diff` command: fuzz two engines, plant a defect and require
//! the fuzzer to find it, or run one case again.

use std::{io::Write, path::PathBuf, time::Duration};

use super::{
    case::Case,
    engines::{Candidate, Fault, Reference, StandIn},
    fuzz::{fuzz, Finding, Progress, Settings},
    pair::{quoted, run, Side, Stored},
    shrink::{shrink, Shrunk},
};
use crate::{
    golden::{record::on_a_deep_stack, Error},
    workload::StreamEngine,
};

/// How the command is used.
pub const USAGE: &str = "\
usage: lignin-diff <command> [options]

commands:
  fuzz      apply random operation streams to the reference engine and to
            the candidate, and compare them after every step, until they
            differ or the time is up
  seeded    plant a defect in the stand-in engine and require the fuzzer to
            find it in time: the acceptance test of the fuzzer itself
  case      run the case of one seed again

options:
  --seed N            the seed of the run, or for case the seed of the case,
                      as a run reports it (default: 1)
  --duration SECONDS  how long the run lasts (default: 60)
  --cases N           fuzz: how many cases the run has, in place of a
                      duration
  --jobs N            how many cases run at once (default: one per
                      processor, four at most)
  --scratch DIR       where databases are created (default: the temporary
                      directory)
  --grove-version N   only cases of this grove version
  --max-ops N         the most operations a case takes from one workload
                      (default: 1500)
  --stored-every N    compare the records the engines store after every N
                      operations that are not reads, and after the last
                      (default: 32; 0 for after the last only)
  --fault FAULT       seeded, case: the defect planted in the stand-in
                      engine, which is then the candidate: added-byte:N,
                      loaded-byte:N, panic:N or unreclaimed (seeded:
                      added-byte:71 unless one is given)
  --shrink-seconds N  how long the shrinking of a stream the engines differ
                      on may take (default: 300)
  --no-shrink         leave a stream the engines differ on as it is

A case is a function of its seed and of --grove-version and --max-ops, so
`case` takes the two as the run had them. fuzz and case exit with 0 when the
engines answered the same and with 1 when they differ. seeded exits with 0
when the defect was found in time and with 1 when it was not.
";

/// The defect `seeded` plants unless another is named: one byte too many
/// for a write that adds 71 bytes. Of the writes the streams make, about
/// one in 4,600 adds as many (40 of 185,138, over the 240 cases of one
/// run).
pub const SEEDED_FAULT: Fault = Fault::AddedByte { when_added: 71 };

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Fuzz,
    Seeded,
    Case,
}

#[derive(Clone, Debug)]
struct Invocation {
    command: Command,
    settings: Settings,
    fault: Option<Fault>,
}

fn parse(arguments: &[String]) -> Result<Invocation, String> {
    let mut arguments = arguments.iter();
    let command = match arguments.next().map(String::as_str) {
        Some("fuzz") => Command::Fuzz,
        Some("seeded") => Command::Seeded,
        Some("case") => Command::Case,
        Some(other) => return Err(format!("`{other}` is not a command")),
        None => return Err("no command given".to_string()),
    };
    let mut settings = Settings::default();
    let mut fault = None;
    let mut grove_version = None;
    let mut shrink_time = None;
    while let Some(option) = arguments.next() {
        let mut value = || {
            arguments
                .next()
                .cloned()
                .ok_or_else(|| format!("`{option}` needs a value"))
        };
        let number = |value: String| {
            let parsed = match value.strip_prefix("0x") {
                Some(hex) => u64::from_str_radix(hex, 16),
                None => value.parse::<u64>(),
            };
            parsed.map_err(|_| format!("`{value}` is not a number"))
        };
        match option.as_str() {
            "--seed" => settings.seed = number(value()?)?,
            "--duration" => settings.duration = Some(Duration::from_secs(number(value()?)?)),
            "--cases" => settings.cases = Some(number(value()?)?),
            "--jobs" => settings.jobs = number(value()?)?.max(1) as usize,
            "--scratch" => settings.scratch = PathBuf::from(value()?),
            "--grove-version" => grove_version = Some(number(value()?)?),
            "--max-ops" => settings.shape.max_ops_per_stream = number(value()?)?.max(1) as usize,
            "--stored-every" => {
                settings.stored = match number(value()?)? {
                    0 => Stored::AtTheEnd,
                    every => Stored::Every(every),
                }
            }
            "--fault" => fault = Some(value()?.parse::<Fault>()?),
            "--shrink-seconds" => shrink_time = Some(Duration::from_secs(number(value()?)?)),
            "--no-shrink" => settings.shrink = None,
            other => return Err(format!("`{other}` is not an option")),
        }
    }
    if let (Some(budget), Some(time)) = (settings.shrink.as_mut(), shrink_time) {
        budget.time = time;
    }
    if let Some(number) = grove_version {
        let number =
            u32::try_from(number).map_err(|_| format!("`{number}` is no grove version"))?;
        settings.shape = settings.shape.for_grove_version(number)?;
    }
    match command {
        Command::Fuzz if fault.is_some() => {
            return Err(
                "`--fault` is an option of seeded and case: fuzz compares the engines as they are"
                    .to_string(),
            );
        }
        Command::Seeded | Command::Case if settings.cases.is_some() => {
            return Err("`--cases` is an option of fuzz".to_string());
        }
        Command::Case if settings.duration.is_some() => {
            return Err("`--duration` is an option of fuzz and seeded".to_string());
        }
        Command::Fuzz | Command::Seeded
            if settings.cases.is_none() && settings.duration.is_none() =>
        {
            settings.duration = Some(Duration::from_secs(60));
        }
        _ => {}
    }
    Ok(Invocation {
        command,
        settings,
        fault,
    })
}

/// The name of an engine in a report: the name of its type.
fn engine_name<E>() -> &'static str {
    let name = std::any::type_name::<E>();
    name.rsplit("::").next().unwrap_or(name)
}

/// The most memory this process has held, where the system says.
fn peak_memory_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    let line = status.lines().find(|line| line.starts_with("VmHWM:"))?;
    let kilobytes: u64 = line.split_whitespace().nth(1)?.parse().ok()?;
    Some(kilobytes * 1024)
}

fn mebibytes(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / f64::from(1 << 20))
}

fn seconds(duration: Duration) -> String {
    format!("{:.1} s", duration.as_secs_f64())
}

fn progress_line(progress: &Progress) -> String {
    let tally = &progress.tally;
    let rate = tally.steps as f64 / progress.elapsed.as_secs_f64().max(0.001);
    format!(
        "{}: {} cases, {} steps ({rate:.0} a second), {} operations failed alike, {} proofs, {} \
         comparisons of {} stored records, {} cases cut short",
        seconds(progress.elapsed),
        progress.cases,
        tally.steps,
        tally.failed,
        tally.proofs,
        tally.walks,
        tally.records,
        tally.cut_short,
    )
}

/// Writes out a stream that was shrunk: every operation it is left with,
/// and how the engines differ on it.
fn write_shrunk(out: &mut dyn Write, shrunk: &Shrunk, reference: &str, candidate: &str) {
    if !shrunk.reproduced {
        let _ = writeln!(
            out,
            "the divergence did not show when the case was run again: the engines do not answer \
             the same stream the same way twice. Nothing was shrunk."
        );
        return;
    }
    let _ = writeln!(
        out,
        "shrunk to {} operations in {} runs and {}{}:",
        shrunk.case.ops.len(),
        shrunk.runs,
        seconds(shrunk.took),
        if shrunk.minimal {
            "; none of them can be left out"
        } else {
            "; the budget ran out before every operation was tried"
        }
    );
    for (index, op) in shrunk.case.ops.iter().enumerate() {
        let _ = writeln!(out, "  {index}: {}", quoted(op));
    }
    for line in shrunk.divergence.report(&shrunk.case, reference, candidate) {
        let _ = writeln!(out, "{line}");
    }
}

fn write_finding(out: &mut dyn Write, finding: &Finding, reference: &str, candidate: &str) {
    let _ = writeln!(
        out,
        "DIVERGENCE after {}, in case {} of the run:",
        seconds(finding.after),
        finding.index
    );
    for line in finding.case.recipe() {
        let _ = writeln!(out, "  {line}");
    }
    for line in finding
        .divergence
        .report(&finding.case, reference, candidate)
    {
        let _ = writeln!(out, "{line}");
    }
    if let Some(shrunk) = &finding.shrunk {
        write_shrunk(out, shrunk, reference, candidate);
    }
    if let Some(why) = &finding.not_shrunk {
        let _ = writeln!(out, "the stream was not shrunk: {why}");
    }
    let _ = writeln!(
        out,
        "to run the case again: lignin-diff case --seed {:#x} (with the --grove-version, \
         --max-ops, --stored-every and --fault of this run)",
        finding.case.seed
    );
}

/// Fuzzes the two engines and says what came of it. Returns whether the
/// engines differed, and after how long.
fn fuzz_and_report<A: StreamEngine, B: StreamEngine>(
    reference: &Side<A>,
    candidate: &Side<B>,
    settings: &Settings,
    out: &mut dyn Write,
) -> Result<Option<Duration>, Error> {
    let versions: Vec<String> = settings
        .shape
        .grove_versions
        .iter()
        .map(|grove_version| grove_version.protocol_version.to_string())
        .collect();
    let _ = writeln!(
        out,
        "{} against {}: seed {:#x}, {} at once, grove versions {}, {}",
        reference.name(),
        candidate.name(),
        settings.seed,
        settings.jobs,
        versions.join(" "),
        match (settings.cases, settings.duration) {
            (Some(cases), _) => format!("{cases} cases"),
            (None, Some(duration)) => format!("for {}", seconds(duration)),
            (None, None) => "until they differ".to_string(),
        }
    );
    let summary = fuzz(reference, candidate, settings, &mut |progress| {
        let _ = writeln!(out, "  {}", progress_line(progress));
        let _ = out.flush();
    })?;
    let _ = writeln!(out, "  {}", progress_line(&summary.progress));
    if let Some(why) = &summary.first_cut_short {
        let _ = writeln!(out, "  the first case cut short: {why}");
    }
    let _ = writeln!(
        out,
        "  the databases held at most {} on disk{}",
        mebibytes(summary.progress.scratch_bytes),
        peak_memory_bytes().map_or(String::new(), |bytes| format!(
            ", the process at most {} of memory",
            mebibytes(bytes)
        ))
    );
    match &summary.finding {
        Some(finding) => {
            write_finding(out, finding, reference.name(), candidate.name());
            Ok(Some(finding.after))
        }
        None => {
            let _ = writeln!(out, "the engines answered the same");
            Ok(None)
        }
    }
}

/// Runs the case of the seed of `settings` once and says what came of it.
/// Returns whether the engines differed.
fn case_and_report<A: StreamEngine, B: StreamEngine>(
    reference: &Side<A>,
    candidate: &Side<B>,
    settings: &Settings,
    out: &mut dyn Write,
) -> Result<bool, Error> {
    let case = Case::generate(settings.seed, &settings.shape);
    let _ = writeln!(out, "{} against {}:", reference.name(), candidate.name());
    for line in case.recipe() {
        let _ = writeln!(out, "  {line}");
    }
    // A stream that cannot be shrunk is still one the engines differ on.
    let (ran, shrunk) = on_a_deep_stack(|| {
        let ran = run(
            &case,
            reference,
            candidate,
            &settings.scratch,
            settings.stored,
            &|| false,
        )?;
        let shrunk = match (&ran.divergence, settings.shrink) {
            (Some(divergence), Some(budget)) => Some(shrink(
                &case,
                divergence,
                reference,
                candidate,
                &settings.scratch,
                budget,
            )),
            _ => None,
        };
        Ok::<_, Error>((ran, shrunk))
    })?;
    let tally = ran.tally;
    let _ = writeln!(
        out,
        "  {} steps, {} operations failed alike, {} proofs, {} comparisons of {} stored records",
        tally.steps, tally.failed, tally.proofs, tally.walks, tally.records
    );
    if let Some(why) = ran.cut_short {
        let _ = writeln!(out, "  cut short {why}");
    }
    let Some(divergence) = ran.divergence else {
        let _ = writeln!(out, "the engines answered the same");
        return Ok(false);
    };
    for line in divergence.report(&case, reference.name(), candidate.name()) {
        let _ = writeln!(out, "{line}");
    }
    match shrunk {
        Some(Ok(shrunk)) => write_shrunk(out, &shrunk, reference.name(), candidate.name()),
        Some(Err(error)) => {
            let _ = writeln!(out, "the stream was not shrunk: {error}");
        }
        None => {}
    }
    Ok(true)
}

fn execute(invocation: &Invocation, out: &mut dyn Write) -> Result<u8, Error> {
    let Invocation {
        command,
        settings,
        fault,
    } = invocation;
    let reference = Side::<Reference>::of(engine_name::<Reference>());
    let candidate = Side::<Candidate>::of(engine_name::<Candidate>());
    match command {
        Command::Fuzz => {
            let differed = fuzz_and_report(&reference, &candidate, settings, out)?;
            Ok(u8::from(differed.is_some()))
        }
        Command::Case => {
            let differed = match fault {
                Some(fault) => case_and_report(&reference, &StandIn::side(*fault), settings, out)?,
                None => case_and_report(&reference, &candidate, settings, out)?,
            };
            Ok(u8::from(differed))
        }
        Command::Seeded => {
            let fault = fault.unwrap_or(SEEDED_FAULT);
            let found = fuzz_and_report(&reference, &StandIn::side(fault), settings, out)?;
            let within = settings.duration.map_or(String::new(), |duration| {
                format!(" within {}", seconds(duration))
            });
            Ok(match found {
                Some(after) => {
                    let _ = writeln!(
                        out,
                        "the planted fault {fault} was found after {}",
                        seconds(after)
                    );
                    0
                }
                None => {
                    let _ = writeln!(out, "the planted fault {fault} was NOT found{within}");
                    1
                }
            })
        }
    }
}

/// Runs the command and returns its exit code: 0 and 1 as [`USAGE`] says,
/// 2 for a command that could not run.
pub fn run_command(arguments: &[String], out: &mut dyn Write) -> u8 {
    let invocation = match parse(arguments) {
        Ok(invocation) => invocation,
        Err(error) => {
            let _ = writeln!(out, "error: {error}\n\n{USAGE}");
            return 2;
        }
    };
    match execute(&invocation, out) {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(out, "error: {error}");
            2
        }
    }
}

#[cfg(test)]
mod tests {
    use grovedb_version::version::GroveVersion;

    use super::*;
    use crate::{
        diff::{
            fuzz::STORED_EVERY,
            pair::testing::{rocksdb, scratch, stand_in_that_opens},
            shrink::Budget,
            Divergence, Part, Tally,
        },
        workload::Op,
    };

    fn arguments(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    fn ran(line: &str, scratch: &std::path::Path) -> (u8, String) {
        let mut out = Vec::new();
        let line = format!("{line} --scratch {}", scratch.display());
        let code = run_command(&arguments(&line), &mut out);
        let text = String::from_utf8(out).expect("text");
        println!("{text}");
        (code, text)
    }

    #[test]
    fn the_options_are_parsed() {
        let invocation = parse(&arguments(
            "fuzz --seed 0x10 --duration 5 --jobs 0 --scratch /scratch --grove-version 3 \
             --max-ops 0 --stored-every 0 --no-shrink",
        ))
        .expect("an invocation");
        assert_eq!(invocation.command, Command::Fuzz);
        let settings = invocation.settings;
        assert_eq!(settings.seed, 16);
        assert_eq!(settings.duration, Some(Duration::from_secs(5)));
        assert_eq!(settings.cases, None);
        assert_eq!(settings.jobs, 1);
        assert_eq!(settings.scratch, PathBuf::from("/scratch"));
        assert_eq!(settings.shape.grove_versions.len(), 1);
        assert_eq!(settings.shape.grove_versions[0].protocol_version, 3);
        assert_eq!(settings.shape.max_ops_per_stream, 1);
        assert_eq!(settings.stored, Stored::AtTheEnd);
        assert_eq!(settings.shrink, None);
        assert_eq!(invocation.fault, None);

        let cases = parse(&arguments("fuzz --cases 7 --stored-every 4")).expect("an invocation");
        assert_eq!(
            (cases.settings.cases, cases.settings.duration),
            (Some(7), None)
        );
        assert_eq!(cases.settings.stored, Stored::Every(4));
        assert_eq!(cases.settings.shrink, Some(Budget::default()));
        let patient = parse(&arguments("fuzz --shrink-seconds 7")).expect("an invocation");
        assert_eq!(
            patient.settings.shrink,
            Some(Budget {
                time: Duration::from_secs(7),
                ..Budget::default()
            })
        );
        let neither =
            parse(&arguments("fuzz --shrink-seconds 7 --no-shrink")).expect("an invocation");
        assert_eq!(neither.settings.shrink, None);

        let fuzz = parse(&arguments("fuzz")).expect("an invocation");
        assert_eq!(fuzz.settings.duration, Some(Duration::from_secs(60)));
        assert_eq!(fuzz.settings.stored, Stored::Every(STORED_EVERY));
        let seeded = parse(&arguments("seeded --fault loaded-byte:9")).expect("an invocation");
        assert_eq!(seeded.command, Command::Seeded);
        assert_eq!(seeded.settings.duration, Some(Duration::from_secs(60)));
        assert_eq!(seeded.fault, Some(Fault::LoadedByte { when_loaded: 9 }));
        let case = parse(&arguments("case --seed 9")).expect("an invocation");
        assert_eq!(case.command, Command::Case);
        assert_eq!((case.settings.seed, case.settings.duration), (9, None));
    }

    #[test]
    fn a_command_line_that_makes_no_sense_is_refused() {
        for (line, error) in [
            ("", "no command given"),
            ("check", "`check` is not a command"),
            ("fuzz --quick", "`--quick` is not an option"),
            ("fuzz --seed", "`--seed` needs a value"),
            ("fuzz --seed x", "`x` is not a number"),
            ("fuzz --seed 0xzz", "`0xzz` is not a number"),
            ("fuzz --grove-version 99", "there is no grove version 99"),
            (
                "fuzz --grove-version 4294967296",
                "`4294967296` is no grove version",
            ),
            (
                "fuzz --fault panic:1",
                "`--fault` is an option of seeded and case: fuzz compares the engines as they are",
            ),
            ("seeded --cases 3", "`--cases` is an option of fuzz"),
            ("case --cases 3", "`--cases` is an option of fuzz"),
            (
                "case --duration 3",
                "`--duration` is an option of fuzz and seeded",
            ),
            (
                "seeded --fault wrong",
                "`wrong` is not a fault: none, added-byte:N, loaded-byte:N, panic:N or \
                 unreclaimed are faults",
            ),
        ] {
            assert_eq!(parse(&arguments(line)).unwrap_err(), error, "{line}");
        }
        let mut out = Vec::new();
        assert_eq!(run_command(&arguments("check"), &mut out), 2);
        let text = String::from_utf8(out).expect("text");
        assert!(text.starts_with("error: `check` is not a command\n\nusage: lignin-diff"));
    }

    #[test]
    fn a_fuzzing_run_that_finds_nothing_exits_with_zero() {
        let scratch = scratch();
        let (code, text) = ran("fuzz --cases 3 --jobs 2 --max-ops 60", scratch.path());
        assert_eq!(code, 0, "{text}");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(
            lines[0],
            "RocksDbEngine against StandIn: seed 0x1, 2 at once, grove versions 1 2 3 4 5, 3 cases"
        );
        let [.., run, held, verdict] = lines[..] else {
            panic!("{text}")
        };
        assert!(run.contains(": 3 cases, ") && run.contains(" steps ("));
        assert!(run.ends_with(", 0 cases cut short"));
        assert!(held.starts_with("  the databases held at most "));
        assert_eq!(verdict, "the engines answered the same");
    }

    #[test]
    fn a_planted_fault_that_is_found_is_reported_shrunk_and_exits_with_zero() {
        let scratch = scratch();
        let (code, text) = ran(
            "seeded --fault added-byte:0 --duration 600 --shrink-seconds 1000000 --jobs 2 \
             --max-ops 60",
            scratch.path(),
        );
        assert_eq!(code, 0, "{text}");
        assert!(text.contains("RocksDbEngine against StandIn with added-byte:0: seed 0x1"));
        assert!(text.contains("\nDIVERGENCE after "));
        assert!(text.contains(": the engines differ in cost\n"));
        assert!(text.contains("\nshrunk to "));
        assert!(text.contains("; none of them can be left out:\n  0: "));
        assert!(text.contains("\nto run the case again: lignin-diff case --seed 0x"));
        assert!(text.contains("\nthe planted fault added-byte:0 was found after "));

        // The case again, from the seed the run reported.
        let seed = text
            .split("lignin-diff case --seed ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .expect("the seed of the case");
        let again = format!("case --seed {seed} --fault added-byte:0 --max-ops 60");
        let (code, case) = ran(&again, scratch.path());
        assert_eq!(code, 1, "{case}");
        assert!(case.starts_with("RocksDbEngine against StandIn with added-byte:0:\n  seed 0x"));
        assert!(case.contains(": the engines differ in cost\n"));
        assert!(case.contains("\nshrunk to "));
        let (code, unshrunk) = ran(&format!("{again} --no-shrink"), scratch.path());
        assert_eq!(code, 1);
        assert!(!unshrunk.contains("shrunk to"));
        // Without the fault the engines answer that case the same.
        let (code, clean) = ran(&format!("case --seed {seed} --max-ops 60"), scratch.path());
        assert_eq!(code, 0, "{clean}");
        assert!(clean.starts_with("RocksDbEngine against StandIn:\n  seed 0x"));
        assert!(clean.ends_with("the engines answered the same\n"));
    }

    #[test]
    fn a_planted_fault_that_is_not_found_exits_with_one() {
        let scratch = scratch();
        // No batch adds four thousand million bytes.
        let (code, text) = ran(
            "seeded --fault added-byte:4000000000 --duration 1 --jobs 1 --max-ops 40",
            scratch.path(),
        );
        assert_eq!(code, 1, "{text}");
        assert!(text.contains("the engines answered the same\n"));
        assert!(
            text.ends_with("the planted fault added-byte:4000000000 was NOT found within 1.0 s\n")
        );
    }

    #[test]
    fn a_run_that_cannot_start_exits_with_two() {
        let scratch = scratch();
        let missing = scratch.path().join("missing");
        for line in ["fuzz --cases 1", "case --seed 3", "seeded --duration 1"] {
            let (code, text) = ran(line, &missing);
            assert_eq!(code, 2, "{text}");
            assert!(
                text.contains("\nerror: ") && text.contains("missing"),
                "{text}"
            );
        }
    }

    #[test]
    fn a_case_the_candidate_panics_on_says_so() {
        let scratch = scratch();
        let (code, text) = ran(
            "case --seed 3 --fault panic:0 --max-ops 60 --stored-every 0",
            scratch.path(),
        );
        assert_eq!(code, 1, "{text}");
        assert!(text.contains(": the engines differ in panic\n"), "{text}");
        assert!(text.contains("  StandIn with panic:0: panicked: "));
    }

    fn cost_divergence() -> Divergence {
        Divergence {
            step: 0,
            parts: vec![Part::Cost],
            reference: vec!["cost 1".to_string()],
            candidate: vec!["cost 2".to_string()],
        }
    }

    #[test]
    fn a_shrinking_that_does_not_reproduce_or_runs_out_says_so() {
        let mut shrunk = Shrunk {
            case: Case::of(0, GroveVersion::latest(), Vec::new()),
            divergence: cost_divergence(),
            runs: 1,
            took: Duration::from_millis(1500),
            reproduced: false,
            minimal: false,
        };
        let written = |shrunk: &Shrunk| {
            let mut out = Vec::new();
            write_shrunk(&mut out, shrunk, "one", "two");
            String::from_utf8(out).expect("text")
        };
        assert!(written(&shrunk).starts_with("the divergence did not show when the case was run"));
        shrunk.reproduced = true;
        assert_eq!(
            written(&shrunk),
            "shrunk to 0 operations in 1 runs and 1.5 s; the budget ran out before every \
             operation was tried:\nstep 0 of 0: the engines differ in cost\n  one: cost 1\n  \
             two: cost 2\n"
        );
    }

    /// The engines differ on a stream whether or not it can be shrunk.
    #[test]
    fn a_stream_that_cannot_be_shrunk_is_reported_all_the_same() {
        let finding = Finding {
            index: 4,
            case: Case::of(9, GroveVersion::latest(), vec![Op::Commit]),
            divergence: cost_divergence(),
            after: Duration::from_secs(2),
            shrunk: None,
            not_shrunk: Some("no database any more".to_string()),
        };
        let mut out = Vec::new();
        write_finding(&mut out, &finding, "one", "two");
        let text = String::from_utf8(out).expect("text");
        assert!(text.starts_with("DIVERGENCE after 2.0 s, in case 4 of the run:\n  seed 0x9, "));
        assert!(text.contains("\nthe stream was not shrunk: no database any more\n"));
        assert!(text.ends_with(
            "to run the case again: lignin-diff case --seed 0x9 (with the --grove-version, \
             --max-ops, --stored-every and --fault of this run)\n"
        ));

        // The case command: a candidate that opens once, for the run that
        // shows the divergence, and not again for the shrinking.
        let scratch = scratch();
        let settings = Settings {
            seed: 3,
            scratch: scratch.path().to_path_buf(),
            ..Settings::default()
        };
        let once = stand_in_that_opens(1, Fault::AddedByte { when_added: 0 });
        let mut out = Vec::new();
        let differed = case_and_report(&rocksdb(), &once, &settings, &mut out).expect("the case");
        let text = String::from_utf8(out).expect("text");
        assert!(differed, "{text}");
        assert!(text.contains(": the engines differ in cost\n"), "{text}");
        assert!(
            text.ends_with("the stream was not shrunk: the engine failed opening a database of worn out: invalid input: no database any more\n"),
            "{text}"
        );
    }

    #[test]
    fn the_numbers_of_a_report_are_readable() {
        assert_eq!(engine_name::<Reference>(), "RocksDbEngine");
        assert_eq!(engine_name::<StandIn>(), "StandIn");
        assert_eq!(engine_name::<u8>(), "u8");
        assert_eq!(mebibytes(3 << 19), "1.5 MiB");
        assert_eq!(seconds(Duration::from_millis(2540)), "2.5 s");
        if cfg!(target_os = "linux") {
            assert!(peak_memory_bytes().expect("the system says") > 1 << 20);
        }
        let line = progress_line(&Progress {
            elapsed: Duration::from_secs(2),
            cases: 3,
            tally: Tally {
                steps: 400,
                failed: 5,
                proofs: 6,
                walks: 7,
                records: 8,
                cut_short: 9,
            },
            scratch_bytes: 0,
        });
        assert_eq!(
            line,
            "2.0 s: 3 cases, 400 steps (200 a second), 5 operations failed alike, 6 proofs, 7 \
             comparisons of 8 stored records, 9 cases cut short"
        );
    }
}
