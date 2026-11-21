//! The `lignin-crash` command: run the campaign, run one seed again, or
//! plant a defect and require the campaign to find it.

use std::{collections::BTreeMap, io::Write, time::Duration};

use super::{
    campaign::{campaign, Faults, Planted, Report, RunShape, Settings, ACTED},
    check::Kind,
    observe::Sabotage,
};

/// How the command is used.
pub const USAGE: &str = "\
usage: lignin-crash <command> [options]

commands:
  run       run random workloads on the page store, take a crash image at
            every write boundary, recover every state the crash plans leave
            and hold each to the model, until a violation is found or an end
            that the options name is reached
  seed      make the run of one seed again
  planted   plant a defect and require the campaign to find it: the
            acceptance test of the campaign itself

options:
  --seed N            the seed of the first run (default: 1)
  --runs N            how many runs, each with the seed after the one before
                      (default: 1, or no bound with --points or --duration;
                      planted: how many runs a defect may take, default 12)
  --points N          run: end with the run that brings the distinct crash
                      states recovered to N
  --duration SECONDS  run: begin no run after this long
  --jobs N            how many runs are made at once (default: 1)
  --faults KIND       the faults every run injects into the file layer:
                      none, syncs or all (default: by the seed, none for two
                      seeds in four, then syncs, then all)
  --ops N             the operations of one lineage (default: 60)
  --lineages N        the most lineages of one run: the first, and those
                      that go on from a state a crash left (default: 5)
  --depth N           how many crashes deep a lineage can be (default: 2)
  --again N           one state in N is recovered a second time, goes on, is
                      closed and is opened again; 0 for none (default: 8)
  --defect NAME       planted: log-sync, new-log-sync, segment-sync,
                      value-sync, table-sync, manifest-sync, dir-sync or
                      forgotten-hash (default: each in turn)
  --quiet             print no line for a run

A run is a function of its seed and of --faults, --ops, --lineages, --depth
and --again, so `seed` takes them as the campaign had them: a campaign that
found a violation prints the command. run and seed exit with 0 when nothing
was found and with 1 when a violation was. planted exits with 0 when every
defect was found and with 1 when one was not. A defect is found by a
violation of the kind it makes, in a run in which it acted and whose seed
gives no violation with no defect planted.
";

/// A defect that `planted` plants.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Defect {
    /// Its name on the command line.
    pub name: &'static str,
    /// What is planted.
    pub planted: Planted,
    /// The kinds of violation that the defect makes: one of another kind
    /// is not its finding.
    pub kinds: &'static [Kind],
    /// Whether the defect shows in a state that a crash left, and not in
    /// the store that was not crashed.
    pub crash: bool,
}

/// The defects `planted` knows.
///
/// | Name | The defect | Found as |
/// | --- | --- | --- |
/// | `log-sync` | A sync of a log file after the one that made its header durable does nothing: commits are reported durable that are not. | A crash state before a commit that was durable. |
/// | `new-log-sync` | No sync of a log file does anything, the first included. | A crash state that does not recover, is not at one with itself, or is before a durable commit. |
/// | `segment-sync` | The same as `log-sync`, of a segment: a table checkpoint names blocks that are not durable. | A crash state that does not recover or is not at one with itself. |
/// | `value-sync` | The same, of a value-log file: a page points to a value that is not durable. | As `new-log-sync`. |
/// | `table-sync` | No sync of a table checkpoint file does anything. | As `new-log-sync`. |
/// | `manifest-sync` | No sync of the manifest's temporary file does anything: a manifest is renamed into place that is not durable. | As `new-log-sync`. |
/// | `dir-sync` | No sync of the database directory does anything: a manifest that was replaced can come back. | As `new-log-sync`. |
/// | `forgotten-hash` | The model misses one changed hash of each commit that changes a key-value hash. | The store that was not crashed says other than the model. |
pub const DEFECTS: [Defect; 8] = [
    file("log-sync", lost("wal/", 1), &[Kind::NotCommitted]),
    file("new-log-sync", lost("wal/", 0), &CRASH),
    file(
        "segment-sync",
        lost("seg/", 1),
        &[Kind::NotRecovered, Kind::Inconsistent],
    ),
    file("value-sync", lost("val/", 1), &CRASH),
    file("table-sync", lost("table/", 0), &CRASH),
    file("manifest-sync", lost("MANIFEST", 0), &CRASH),
    file("dir-sync", Sabotage::LostDirSync("db"), &CRASH),
    Defect {
        name: "forgotten-hash",
        planted: Planted::ForgottenHash,
        kinds: &[Kind::Session],
        crash: false,
    },
];

/// What a state that a crash left can be found to be.
const CRASH: [Kind; 3] = [Kind::NotRecovered, Kind::NotCommitted, Kind::Inconsistent];

const fn file(name: &'static str, sabotage: Sabotage, kinds: &'static [Kind]) -> Defect {
    Defect {
        name,
        planted: Planted::File(sabotage),
        kinds,
        crash: true,
    }
}

const fn lost(part: &'static str, after: u64) -> Sabotage {
    Sabotage::LostSync { part, after }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Command {
    Run,
    Seed,
    Planted,
}

#[derive(Clone, Debug)]
struct Invocation {
    command: Command,
    settings: Settings,
    defect: Option<Defect>,
    quiet: bool,
}

fn parse(arguments: &[String]) -> Result<Invocation, String> {
    let mut arguments = arguments.iter();
    let command = match arguments.next().map(String::as_str) {
        Some("run") => Command::Run,
        Some("seed") => Command::Seed,
        Some("planted") => Command::Planted,
        Some(other) => return Err(format!("`{other}` is not a command")),
        None => return Err("no command given".to_string()),
    };
    let mut settings = Settings {
        runs: None,
        ..Settings::default()
    };
    let mut shape = RunShape::default();
    let (mut defect, mut quiet) = (None, false);
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
            "--runs" => settings.runs = Some(number(value()?)?.max(1)),
            "--points" => settings.points = Some(number(value()?)?),
            "--duration" => settings.duration = Some(Duration::from_secs(number(value()?)?)),
            "--jobs" => settings.jobs = number(value()?)?.max(1) as usize,
            "--faults" => {
                let named = value()?;
                let all = [Faults::None, Faults::Syncs, Faults::All];
                let faults = all.into_iter().find(|faults| faults.name() == named);
                shape.faults = Some(faults.ok_or(format!("`{named}` is no kind of faults"))?);
            }
            "--ops" => shape.ops = number(value()?)?.max(1) as usize,
            "--lineages" => shape.lineages = number(value()?)?.max(1) as usize,
            "--depth" => shape.depth = number(value()?)? as usize,
            "--again" => shape.again_one_in = number(value()?)?,
            "--defect" => {
                let named = value()?;
                let known = DEFECTS.into_iter().find(|defect| defect.name == named);
                defect = Some(known.ok_or(format!("`{named}` is no defect"))?);
            }
            "--quiet" => quiet = true,
            other => return Err(format!("`{other}` is not an option")),
        }
    }
    let bounded = settings.points.is_some() || settings.duration.is_some();
    match command {
        Command::Run if settings.runs.is_none() && !bounded => settings.runs = Some(1),
        Command::Seed => {
            if bounded || settings.runs.is_some() {
                return Err(
                    "seed makes one run: it takes no --runs, --points or --duration".to_string(),
                );
            }
            settings.runs = Some(1);
        }
        Command::Planted if bounded => {
            return Err("planted takes --runs, not --points or --duration".to_string());
        }
        _ => {}
    }
    if defect.is_some() && command != Command::Planted {
        return Err("`--defect` is an option of planted".to_string());
    }
    settings.shape = shape;
    Ok(Invocation {
        command,
        settings,
        defect,
        quiet,
    })
}

fn progress<'o>(
    out: &'o mut dyn Write,
    quiet: bool,
) -> impl FnMut(&super::campaign::RunReport, u64) + 'o {
    move |run, points| {
        if !quiet {
            let _ = writeln!(
                out,
                "seed {}: {} crash states, {} violations; {points} distinct states so far",
                run.seed,
                run.fingerprints.len(),
                run.violations.len()
            );
        }
    }
}

/// The command that makes the run of `seed` again as a campaign of
/// `shape` made it.
fn again(seed: u64, shape: &RunShape) -> String {
    let usual = RunShape::default();
    let mut line = format!("lignin-crash seed --seed {seed}");
    if let Some(faults) = shape.faults {
        line += &format!(" --faults {}", faults.name());
    }
    for (option, value, usual) in [
        ("--ops", shape.ops as u64, usual.ops as u64),
        ("--lineages", shape.lineages as u64, usual.lineages as u64),
        ("--depth", shape.depth as u64, usual.depth as u64),
        ("--again", shape.again_one_in, usual.again_one_in),
    ] {
        if value != usual {
            line += &format!(" {option} {value}");
        }
    }
    line
}

fn report(out: &mut dyn Write, report: &Report, shape: &RunShape) {
    let _ = writeln!(out, "{report}");
    let seconds = report.elapsed.as_secs_f64().max(1e-9);
    let _ = writeln!(
        out,
        "{:.0} distinct crash states a second",
        report.points as f64 / seconds
    );
    if let Some(found) = report.violations.first() {
        let _ = writeln!(out, "to make that run again: {}", again(found.seed, shape));
    }
}

/// What `planted` says of `defect`, and whether it was found. `done` is
/// its campaign, `acted` how often the defect did what it is planted to do
/// in the run that found something, and `control` says whether the run of
/// a seed is clean with no defect planted.
///
/// A defect is found by a violation of a kind it makes, in a run in which
/// it acted, of a seed that gives no violation without it.
fn verdict(
    defect: &Defect,
    done: &Report,
    acted: u64,
    control: &mut dyn FnMut(u64) -> bool,
) -> (bool, String) {
    let name = defect.name;
    let Some(found) = done.violations.first() else {
        return (false, format!("{name}: NOT FOUND in {} runs", done.runs));
    };
    let its_own =
        defect.kinds.contains(&found.violation.kind) && (found.boundary.is_some() || !defect.crash);
    let why = if acted == 0 {
        "the defect did nothing in that run"
    } else if !its_own {
        "that is not what the defect does"
    } else if !control(found.seed) {
        "the run of that seed has a violation with no defect planted"
    } else {
        let said = format!(
            "{name}: found in run {}, after {:.1} s: {}",
            done.runs,
            done.elapsed.as_secs_f64(),
            found.violation
        );
        return (true, said);
    };
    let said = format!(
        "{name}: NOT FOUND: run {} has a violation, and {why}: {found}",
        done.runs
    );
    (false, said)
}

/// The most runs a planted defect may take to be found, unless `--runs`
/// says another number.
pub const PLANTED_RUNS: u64 = 12;

/// Runs the command `arguments` name, writes what it did to `out`, and
/// returns the exit code.
pub fn run_command(arguments: &[String], out: &mut dyn Write) -> u8 {
    let invocation = match parse(arguments) {
        Ok(invocation) => invocation,
        Err(error) => {
            let _ = writeln!(out, "lignin-crash: {error}\n\n{USAGE}");
            return 2;
        }
    };
    let Invocation {
        command,
        mut settings,
        defect,
        quiet,
    } = invocation;
    match command {
        Command::Run | Command::Seed => {
            let done = campaign(&settings, &mut progress(out, quiet));
            report(out, &done, &settings.shape);
            u8::from(!done.clean())
        }
        Command::Planted => {
            let defects: Vec<_> = match defect {
                Some(defect) => vec![defect],
                None => DEFECTS.to_vec(),
            };
            settings.runs = Some(settings.runs.unwrap_or(PLANTED_RUNS));
            // A planted defect is to be found by a store that meets
            // nothing else: every error is then the defect's.
            settings.shape.faults = Some(Faults::None);
            let clean = Settings {
                runs: Some(1),
                jobs: 1,
                ..settings.clone()
            };
            let mut controls: BTreeMap<u64, bool> = BTreeMap::new();
            let mut control = |seed| {
                let run = || {
                    campaign(
                        &Settings {
                            seed,
                            ..clean.clone()
                        },
                        &mut |_, _| {},
                    )
                    .clean()
                };
                *controls.entry(seed).or_insert_with(run)
            };
            let mut missed = 0;
            for defect in defects {
                settings.shape.planted = Some(defect.planted);
                let mut acted = 0;
                let done = campaign(&settings, &mut |run, _| {
                    if !run.violations.is_empty() {
                        acted = run.tally.get(ACTED);
                    }
                });
                let (found, said) = verdict(&defect, &done, acted, &mut control);
                let _ = writeln!(out, "{said}");
                missed += u32::from(!found);
            }
            u8::from(missed > 0)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_string).collect()
    }

    fn said(line: &str) -> (u8, String) {
        let mut out = Vec::new();
        let code = run_command(&arguments(line), &mut out);
        (code, String::from_utf8(out).expect("text"))
    }

    #[test]
    fn the_options_set_the_campaign() {
        let invocation = parse(&arguments(
            "run --seed 0x10 --runs 3 --points 500 --duration 9 --jobs 2 --faults syncs \
             --ops 7 --lineages 2 --depth 1 --again 0 --quiet",
        ))
        .expect("parsed");
        let Settings {
            seed,
            runs,
            points,
            duration,
            jobs,
            shape,
        } = invocation.settings;
        assert_eq!((seed, runs, points, jobs), (16, Some(3), Some(500), 2));
        assert_eq!(duration, Some(Duration::from_secs(9)));
        assert_eq!(shape.faults, Some(Faults::Syncs));
        assert_eq!((shape.ops, shape.lineages, shape.depth), (7, 2, 1));
        assert_eq!(shape.again_one_in, 0);
        assert!(invocation.quiet && invocation.command == Command::Run);

        // A run with no end named is one run, and one with an end has no
        // bound on its runs.
        assert_eq!(
            parse(&arguments("run")).expect("parsed").settings.runs,
            Some(1)
        );
        let bounded = parse(&arguments("run --points 5")).expect("parsed");
        assert_eq!(bounded.settings.runs, None);
        assert_eq!(
            parse(&arguments("seed --seed 4"))
                .expect("parsed")
                .settings
                .runs,
            Some(1)
        );
        let planted = parse(&arguments("planted --defect dir-sync")).expect("parsed");
        assert_eq!(planted.defect.map(|defect| defect.name), Some("dir-sync"));
    }

    #[test]
    fn what_is_not_a_command_line_is_refused_with_the_usage() {
        for line in [
            "",
            "crash",
            "run --seed",
            "run --seed x",
            "run --faults some",
            "run --fast",
            "run --defect log-sync",
            "planted --defect none",
            "planted --points 4",
            "seed --runs 2",
            "seed --duration 2",
        ] {
            let (code, text) = said(line);
            assert_eq!(code, 2, "{line}");
            assert!(text.contains("usage: lignin-crash"), "{line}: {text}");
        }
    }

    #[test]
    fn a_run_reports_what_it_did_and_exits_with_zero_when_it_is_clean() {
        let (code, text) = said("run --seed 1 --ops 6 --lineages 1 --again 0");
        assert_eq!(code, 0, "{text}");
        assert!(text.contains("seed 1: "), "{text}");
        assert!(text.contains("1 runs (seeds 1 to 1)"), "{text}");
        assert!(text.contains("distinct crash states a second"), "{text}");
        let (code, quiet) = said("seed --seed 1 --ops 6 --lineages 1 --again 0 --quiet");
        assert_eq!(code, 0);
        assert!(!quiet.contains("seed 1: "), "{quiet}");
        // The same seed, the same digest.
        let digest = |text: &str| {
            text.lines()
                .find(|line| line.starts_with("digest"))
                .map(str::to_string)
        };
        assert_eq!(digest(&text), digest(&quiet));
        assert!(digest(&text).is_some());
    }

    #[test]
    fn a_violation_comes_with_the_command_that_makes_its_run_again() {
        use crate::crash::{campaign::Found, check::Violation};

        let shape = RunShape {
            faults: Some(Faults::Syncs),
            ops: 9,
            again_one_in: 0,
            ..RunShape::default()
        };
        let line = again(7, &shape);
        assert_eq!(
            line,
            "lignin-crash seed --seed 7 --faults syncs --ops 9 --again 0"
        );
        let command = line.strip_prefix("lignin-crash ").expect("the command");
        let parsed = parse(&arguments(command)).expect("parsed");
        assert_eq!((parsed.settings.seed, parsed.settings.shape), (7, shape));
        assert_eq!(again(3, &RunShape::default()), "lignin-crash seed --seed 3");

        let found = Found {
            seed: 7,
            lineage: 1,
            op: 4,
            boundary: Some(90),
            plan: Some("a plan".to_string()),
            violation: Violation::new(Kind::NotCommitted, "below the floor"),
            trail: vec!["commit (new)".to_string()],
        };
        let mut done = Report {
            first_seed: 5,
            runs: 3,
            violations: vec![found],
            ..Report::default()
        };
        let mut out = Vec::new();
        report(&mut out, &done, &shape);
        let text = String::from_utf8(out).expect("text");
        assert!(text.contains("VIOLATION: seed 7, lineage 1"), "{text}");
        assert!(
            text.ends_with(&format!("to make that run again: {line}\n")),
            "{text}"
        );

        // A planted defect is found by a violation of its kind, in a run
        // in which it acted, of a seed that is clean without it.
        let log_sync = DEFECTS[0];
        assert_eq!(log_sync.name, "log-sync");
        let mut asked = Vec::new();
        let mut clean = |seed| {
            asked.push(seed);
            true
        };
        let (found, said) = verdict(&log_sync, &done, 2, &mut clean);
        assert!(
            found && said.starts_with("log-sync: found in run 3, "),
            "{said}"
        );
        let (found, said) = verdict(&log_sync, &done, 0, &mut clean);
        assert!(
            !found && said.contains("the defect did nothing in that run"),
            "{said}"
        );
        assert_eq!(asked, [7]);
        let (found, said) = verdict(&log_sync, &done, 2, &mut |_| false);
        assert!(!found && said.contains("with no defect planted"), "{said}");
        // A violation of another kind, or one of a store that was not
        // crashed, is not what a lost sync of the log does.
        let forgotten = DEFECTS[7];
        let (found, said) = verdict(&forgotten, &done, 2, &mut |_| true);
        assert!(
            !found && said.contains("not what the defect does"),
            "{said}"
        );
        done.violations[0].boundary = None;
        let (found, said) = verdict(&log_sync, &done, 2, &mut |_| true);
        assert!(
            !found && said.contains("not what the defect does"),
            "{said}"
        );
        done.violations.clear();
        let (found, said) = verdict(&log_sync, &done, 0, &mut |_| true);
        assert!(!found && said == "log-sync: NOT FOUND in 3 runs", "{said}");

        // A report of runs whose seeds go past the last number.
        let wrapped = Report {
            first_seed: u64::MAX,
            runs: 2,
            ..Report::default()
        };
        let said = wrapped.to_string();
        assert!(
            said.contains(&format!("(seeds {} to 0)", u64::MAX)),
            "{said}"
        );
    }

    #[test]
    fn a_planted_defect_that_is_found_exits_with_zero() {
        let (code, text) = said("planted --defect forgotten-hash --ops 40 --lineages 1");
        assert_eq!(code, 0, "{text}");
        assert!(text.starts_with("forgotten-hash: found in run"), "{text}");
        // With one operation a run, no commit changes a hash.
        let (code, text) = said("planted --defect forgotten-hash --ops 1 --lineages 1 --runs 2");
        assert_eq!(code, 1, "{text}");
        assert!(
            text.contains("forgotten-hash: NOT FOUND in 2 runs"),
            "{text}"
        );
    }
}
