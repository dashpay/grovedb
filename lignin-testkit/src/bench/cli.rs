//! The `lignin-bench` command.
//!
//! ```text
//! lignin-bench run [options]          measure, write the report as JSON
//! lignin-bench table REPORT           the rows of a report, beside the plan's values
//! lignin-bench compare FIRST SECOND   how far two runs' rows are apart
//! ```
//!
//! `run` options: `--out FILE` (default: standard output), `--scratch DIR`
//! (default: `$TMPDIR/lignin-bench`), `--engine NAME` (repeatable; default
//! `rocksdb`), `--label TEXT`, `--commit SHA`, `--perf`, `--keep`, and the
//! sizes of [`Settings`]: `--seed`, `--documents`, `--owners`, `--creates`,
//! `--updates`, `--deletes`, `--point-reads`, `--reference-reads`,
//! `--warmup-reads`, `--cold-reads`, `--checkpoints`, `--reopens`.
//! `compare` takes `--tolerance FRACTION` (default 0.10).

use std::{fmt::Write as _, path::PathBuf};

use grovedb_version::version::GroveVersion;

use super::{
    compare,
    report::{self, ratio},
    run, BenchError, EngineChoice, Report, RowKind, RunOptions, Settings,
};

const USAGE: &str =
    "usage: lignin-bench run [options] | table REPORT | compare FIRST SECOND [--tolerance F]";

/// Runs the command with `args` (the program name excluded) and returns
/// what it prints.
pub fn main_with(args: &[String]) -> Result<String, BenchError> {
    let usage = || BenchError::Check(USAGE.to_string());
    let (command, rest) = args.split_first().ok_or_else(usage)?;
    match command.as_str() {
        "run" => {
            let (options, out) = parse_run(rest)?;
            let report = run(&options)?;
            let json = report.to_json();
            match out {
                Some(path) => {
                    std::fs::write(&path, &json).map_err(|error| {
                        BenchError::io(format!("writing {}", path.display()), error)
                    })?;
                    Ok(table(&report))
                }
                None => Ok(json),
            }
        }
        "table" => match rest {
            [path] => Ok(table(&read_report(path)?)),
            _ => Err(usage()),
        },
        "compare" => {
            let (paths, tolerance) = match rest {
                [first, second] => ([first, second], 0.10),
                [first, second, flag, value] if flag == "--tolerance" => {
                    ([first, second], parse_number(flag, value)?)
                }
                _ => return Err(usage()),
            };
            comparison(&read_report(paths[0])?, &read_report(paths[1])?, tolerance)
        }
        _ => Err(usage()),
    }
}

fn read_report(path: &str) -> Result<Report, BenchError> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| BenchError::io(format!("reading {path}"), error))?;
    Report::from_json(&text)
}

fn parse_number<T: std::str::FromStr>(flag: &str, value: &str) -> Result<T, BenchError> {
    value
        .parse()
        .map_err(|_| BenchError::Check(format!("{flag} takes a number, not {value:?}")))
}

/// The options of `run`, and the file to write the report to.
pub fn parse_run(args: &[String]) -> Result<(RunOptions, Option<PathBuf>), BenchError> {
    let scratch = std::env::temp_dir().join("lignin-bench");
    let mut options = RunOptions {
        settings: Settings::reference(scratch),
        engines: Vec::new(),
        machine_label: None,
        commit: None,
        grove_version: GroveVersion::latest(),
    };
    let mut out = None;
    let mut args = args.iter();
    while let Some(flag) = args.next() {
        let settings = &mut options.settings;
        match flag.as_str() {
            "--perf" => settings.perf = true,
            "--keep" => settings.keep = true,
            _ => {
                let value = args
                    .next()
                    .ok_or_else(|| BenchError::Check(format!("{flag} needs a value")))?;
                match flag.as_str() {
                    "--out" => out = Some(PathBuf::from(value)),
                    "--scratch" => settings.scratch = PathBuf::from(value),
                    "--label" => options.machine_label = Some(value.clone()),
                    "--commit" => options.commit = Some(value.clone()),
                    "--engine" => options.engines.push(
                        EngineChoice::named(value)
                            .ok_or_else(|| BenchError::Check(format!("no engine {value:?}")))?,
                    ),
                    "--seed" => settings.seed = parse_number(flag, value)?,
                    "--documents" => settings.documents = parse_number(flag, value)?,
                    "--owners" => settings.owners = parse_number(flag, value)?,
                    "--creates" => settings.measured_creates = parse_number(flag, value)?,
                    "--updates" => settings.updates = parse_number(flag, value)?,
                    "--deletes" => settings.deletes = parse_number(flag, value)?,
                    "--point-reads" => settings.point_reads = parse_number(flag, value)?,
                    "--reference-reads" => settings.reference_reads = parse_number(flag, value)?,
                    "--warmup-reads" => settings.warmup_reads = parse_number(flag, value)?,
                    "--cold-reads" => settings.cold_reads = parse_number(flag, value)?,
                    "--checkpoints" => settings.checkpoints = parse_number(flag, value)?,
                    "--reopens" => settings.reopens = parse_number(flag, value)?,
                    _ => return Err(BenchError::Check(format!("unknown option {flag}"))),
                }
            }
        }
    }
    if options.engines.is_empty() {
        options.engines.push(EngineChoice::RocksDb);
    }
    options.settings.check()?;
    Ok((options, out))
}

fn number(value: f64) -> String {
    if value.abs() >= 100.0 || value == value.trunc() {
        format!("{value:.0}")
    } else if value.abs() >= 10.0 {
        format!("{value:.1}")
    } else {
        format!("{value:.3}")
    }
}

fn percent(fraction: Option<f64>) -> String {
    fraction.map_or_else(
        || "-".to_string(),
        |fraction| format!("{:+.1}%", fraction * 100.0),
    )
}

/// The rows of `report`, one line each, beside the plan's recorded values.
pub fn table(report: &Report) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} {} on {} ({}), commit {}, load {:?} -> {:?}",
        report.workload.name,
        report.workload.documents,
        report
            .machine
            .label
            .as_deref()
            .unwrap_or(&report.machine.os),
        report.machine.cpu.as_deref().unwrap_or("unknown cpu"),
        report.commit.as_deref().unwrap_or("unknown"),
        report.load_average_before,
        report.load_average_after,
    );
    for engine in &report.engines {
        let _ = writeln!(
            out,
            "{:<9} {:<8} {:<36} {:>14} {:<7} {:>12} {:>9} {:>14} {:>9}",
            "engine",
            "phase",
            "row",
            "value",
            "unit",
            "recorded",
            "vs plan",
            "instr/sample",
            "vs rocks"
        );
        for row in &engine.rows {
            let _ = writeln!(
                out,
                "{:<9} {:<8} {:<36} {:>14} {:<7} {:>12} {:>9} {:>14} {:>9}",
                engine.engine,
                row.phase,
                row.id,
                number(row.value),
                row.unit,
                row.recorded
                    .as_ref()
                    .map_or_else(|| "-".to_string(), |recorded| number(recorded.value)),
                percent(row.ratio_to_recorded.map(|ratio| ratio - 1.0)),
                row.instructions_per_sample
                    .map_or_else(|| "-".to_string(), number),
                row.ratio_to_rocksdb
                    .map_or_else(|| "-".to_string(), |ratio| format!("{ratio:.3}")),
            );
        }
    }
    out
}

/// How far the rows of two runs are apart, and how many agree within
/// `tolerance`.
pub fn comparison(first: &Report, second: &Report, tolerance: f64) -> Result<String, BenchError> {
    report::comparable(first, second)?;
    let mut out = String::new();
    for (engine, id, phase) in report::unmatched(first, second) {
        let _ = writeln!(
            out,
            "{engine:<9} {phase:<8} {id:<36} measured in one run only"
        );
    }
    let agreements = compare(first, second);
    let mut within = [0usize; 3];
    let mut total = [0usize; 3];
    let verdict = |agrees: bool| if agrees { "ok" } else { "APART" };
    for agreement in &agreements {
        let slot = usize::from(agreement.kind == RowKind::Time);
        total[slot] += 1;
        let agrees = agreement.within(tolerance);
        within[slot] += usize::from(agrees);
        let instructions = agreement.instructions_within(tolerance);
        if let Some(agrees) = instructions {
            total[2] += 1;
            within[2] += usize::from(agrees);
        }
        let _ = writeln!(
            out,
            "{:<9} {:<8} {:<36} {:>14} {:>14} {:<7} {:>8} {:<5} {:>8} {}",
            agreement.engine,
            agreement.phase,
            agreement.id,
            number(agreement.first),
            number(agreement.second),
            agreement.unit,
            percent(agreement.change),
            verdict(agrees),
            percent(agreement.instructions_change),
            instructions.map_or("", verdict),
        );
    }
    let _ = writeln!(
        out,
        "within {}: {} of {} count rows, {} of {} time rows, {} of {} time rows by instructions",
        percent(ratio(tolerance, 1.0)),
        within[0],
        total[0],
        within[1],
        total[1],
        within[2],
        total[2]
    );
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn run_options_parse() {
        let (options, out) = parse_run(&args(
            "--out r.json --scratch /s --label box --commit abc --engine rocksdb --perf --keep \
             --seed 7 --documents 2000 --owners 20 --creates 40 --updates 60 --deletes 80 \
             --point-reads 1 --reference-reads 2 --warmup-reads 3 --cold-reads 4 \
             --checkpoints 5 --reopens 6",
        ))
        .unwrap();
        assert_eq!(out, Some(PathBuf::from("r.json")));
        assert_eq!(options.engines, vec![EngineChoice::RocksDb]);
        assert_eq!(options.machine_label.as_deref(), Some("box"));
        assert_eq!(options.commit.as_deref(), Some("abc"));
        let settings = &options.settings;
        assert_eq!(settings.scratch, PathBuf::from("/s"));
        assert!(settings.perf && settings.keep);
        assert_eq!(
            (
                settings.seed,
                settings.documents,
                settings.owners,
                settings.measured_creates,
                settings.updates,
                settings.deletes
            ),
            (7, 2000, 20, 40, 60, 80)
        );
        assert_eq!(
            (
                settings.point_reads,
                settings.reference_reads,
                settings.warmup_reads,
                settings.cold_reads,
                settings.checkpoints,
                settings.reopens
            ),
            (1, 2, 3, 4, 5, 6)
        );

        let (defaults, out) = parse_run(&[]).unwrap();
        assert_eq!(out, None);
        assert_eq!(defaults.engines, vec![EngineChoice::RocksDb]);
        assert_eq!(defaults.settings.documents, 1_000_000);
    }

    #[test]
    fn bad_arguments_are_refused() {
        for (line, message) in [
            ("--seed", "--seed needs a value"),
            ("--seed x", "--seed takes a number, not \"x\""),
            ("--engine sled", "no engine \"sled\""),
            ("--frobnicate 1", "unknown option --frobnicate"),
            (
                "--documents 30",
                "documents (30) must be a multiple of 20, the transitions per block",
            ),
        ] {
            assert_eq!(parse_run(&args(line)).unwrap_err().to_string(), message);
        }
        for line in ["", "fly", "table", "compare a", "compare a b --tolerance"] {
            assert_eq!(main_with(&args(line)).unwrap_err().to_string(), USAGE);
        }
        assert!(main_with(&args("table /nonexistent/report.json"))
            .unwrap_err()
            .to_string()
            .starts_with("reading /nonexistent/report.json"));
        assert_eq!(
            main_with(&args("compare a b --tolerance x"))
                .unwrap_err()
                .to_string(),
            "--tolerance takes a number, not \"x\""
        );
    }

    #[test]
    fn numbers_print_to_their_precision() {
        assert_eq!(number(23_912.4), "23912");
        assert_eq!(number(58.84), "58.8");
        assert_eq!(number(8.9123), "8.912");
        assert_eq!(number(3.0), "3");
        assert_eq!(percent(Some(-0.0512)), "-5.1%");
        assert_eq!(percent(None), "-");
    }
}
