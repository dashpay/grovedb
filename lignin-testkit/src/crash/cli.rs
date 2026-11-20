//! The `lignin-crash` command: run the campaign, run one seed again, or
//! plant a defect and require the campaign to find it.

use std::{io::Write, time::Duration};

use super::{
    campaign::{campaign, Faults, Planted, Report, RunShape, Settings},
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
and --again, so `seed` takes them as the campaign had them. run and seed exit
with 0 when nothing was found and with 1 when a violation was. planted exits
with 0 when every defect was found and with 1 when one was not.
";

/// The defects `planted` knows, by name.
///
/// | Name | The defect |
/// | --- | --- |
/// | `log-sync` | A sync of a log file after the one that made its header durable does nothing: commits are reported durable that are not. |
/// | `new-log-sync` | No sync of a log file does anything, the first included. |
/// | `segment-sync` | The same as `log-sync`, of a segment: a table checkpoint names blocks that are not durable. |
/// | `value-sync` | The same, of a value-log file: a page points to a value that is not durable. |
/// | `table-sync` | No sync of a table checkpoint file does anything. |
/// | `manifest-sync` | No sync of the manifest's temporary file does anything: a manifest is renamed into place that is not durable. |
/// | `dir-sync` | No sync of the database directory does anything: a manifest that was replaced can come back. |
/// | `forgotten-hash` | The model misses one changed hash of each commit that changes a key-value hash. |
pub const DEFECTS: [(&str, Planted); 8] = [
    ("log-sync", Planted::File(lost("wal/", 1))),
    ("new-log-sync", Planted::File(lost("wal/", 0))),
    ("segment-sync", Planted::File(lost("seg/", 1))),
    ("value-sync", Planted::File(lost("val/", 1))),
    ("table-sync", Planted::File(lost("table/", 0))),
    ("manifest-sync", Planted::File(lost("MANIFEST", 0))),
    ("dir-sync", Planted::File(Sabotage::LostDirSync("db"))),
    ("forgotten-hash", Planted::ForgottenHash),
];

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
    defect: Option<(&'static str, Planted)>,
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
                let known = DEFECTS.into_iter().find(|(name, _)| *name == named);
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

fn report(out: &mut dyn Write, report: &Report) {
    let _ = writeln!(out, "{report}");
    let seconds = report.elapsed.as_secs_f64().max(1e-9);
    let _ = writeln!(
        out,
        "{:.0} distinct crash states a second",
        report.points as f64 / seconds
    );
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
            report(out, &done);
            u8::from(!done.clean())
        }
        Command::Planted => {
            let defects: Vec<_> = match defect {
                Some(defect) => vec![defect],
                None => DEFECTS.to_vec(),
            };
            settings.runs = Some(settings.runs.unwrap_or(PLANTED_RUNS));
            let mut missed = 0;
            for (name, planted) in defects {
                // A planted defect is to be found by a store that meets
                // nothing else: every error is then the defect's.
                settings.shape.faults = Some(Faults::None);
                settings.shape.planted = Some(planted);
                let done = campaign(&settings, &mut |_, _| {});
                match done.violations.first() {
                    Some(found) => {
                        let _ = writeln!(
                            out,
                            "{name}: found in run {} of {}, after {:.1} s: {}",
                            done.runs,
                            settings.runs.unwrap_or(0),
                            done.elapsed.as_secs_f64(),
                            found.violation
                        );
                    }
                    None => {
                        missed += 1;
                        let _ = writeln!(out, "{name}: NOT FOUND in {} runs", done.runs);
                    }
                }
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
        assert_eq!(planted.defect.map(|(name, _)| name), Some("dir-sync"));
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
