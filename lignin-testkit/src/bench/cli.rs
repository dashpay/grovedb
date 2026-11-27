//! The `lignin-bench` command.
//!
//! ```text
//! lignin-bench run [options]          measure, write the report as JSON
//! lignin-bench reads [options]        the warm read rows of a kept database
//! lignin-bench writes [options]       the write rows over a copy of a kept database
//! lignin-bench table REPORT           the rows of a report, beside the plan's values
//! lignin-bench compare FIRST SECOND   how far two runs' rows are apart
//! lignin-bench latency [options]      the commit of a block at a pace, on each engine
//! lignin-bench latency-table REPORT.. the rows of latency reports, and their ranges
//! lignin-bench score REPORT...        the rows of reports scored against the plan's table
//! ```
//!
//! `run` options: `--out FILE` (default: standard output), `--scratch DIR`
//! (default: `$TMPDIR/lignin-bench`), `--engine NAME` (`rocksdb` or
//! `lignindb`; repeatable, measured in the order named; default `rocksdb`),
//! `--set KEY=VALUE` (repeatable; a setting of LigninDB, as
//! `bench/lignindb.rs` lists them, in a run that measures it),
//! `--label TEXT`, `--commit SHA`, `--perf`, `--keep`, and the
//! sizes of [`Settings`]: `--seed`, `--documents`, `--owners`, `--churn`, `--creates`,
//! `--updates`, `--deletes`, `--point-reads`, `--reference-reads`,
//! `--warmup-reads`, `--cold-reads`, `--checkpoints`, `--reopens`.
//! `reads` takes the options of `run` and measures the point gets, the
//! point proofs and the proofs of 100 references, as rows of the phase
//! `settled`, over the databases that a
//! `run --keep` with the same `--scratch`, `--seed`, `--documents` and
//! `--owners` and with `--creates 0 --updates 0 --deletes 0` left there.
//! `writes` takes the options of `run` too, and applies its `--creates`,
//! `--updates` and `--deletes` to a copy of such a kept database, opened
//! with its `--set` settings: the rows of the phases `create`, `update`
//! and `delete`. With `--keep` the copy stays, in `<engine>-writes` beside
//! the kept database.
//! `compare` takes `--tolerance FRACTION` (default 0.10). The options of
//! `latency` are those of [`latency`].

use std::{fmt::Write as _, path::PathBuf};

use grovedb_version::version::GroveVersion;

use super::{
    compare,
    latency::{self, LatencyReport},
    reads,
    report::{self, ratio},
    run, score, writes, BenchError, EngineChoice, Report, RowKind, RunOptions, Settings,
};

const USAGE: &str = "usage: lignin-bench run [options] | reads [options] | writes [options] | \
                     table REPORT | compare FIRST SECOND [--tolerance F] | latency [options] | \
                     latency-table REPORT... | score REPORT...";

/// Runs the command with `args` (the program name excluded) and returns
/// what it prints.
pub fn main_with(args: &[String]) -> Result<String, BenchError> {
    let usage = || BenchError::Check(USAGE.to_string());
    let (command, rest) = args.split_first().ok_or_else(usage)?;
    match command.as_str() {
        "run" | "reads" | "writes" => {
            let (options, out) = parse_run(rest)?;
            let report = match command.as_str() {
                "run" => run(&options)?,
                "reads" => reads(&options)?,
                _ => writes(&options)?,
            };
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
        "score" if !rest.is_empty() => {
            let mut out = String::new();
            for path in rest {
                let report = read_report(path)?;
                let _ = writeln!(
                    out,
                    "{path}: {} {}, commit {}\n",
                    report.workload.name,
                    report.workload.documents,
                    report.commit.as_deref().unwrap_or("unknown"),
                );
                out.push_str(&score::score(&report));
                out.push('\n');
            }
            Ok(out)
        }
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
        "latency" => {
            let (options, out) = latency::parse_options(rest)?;
            let report = latency::run(&options)?;
            match out {
                Some(path) => {
                    std::fs::write(&path, report.to_json()).map_err(|error| {
                        BenchError::io(format!("writing {}", path.display()), error)
                    })?;
                    Ok(latency::table(&report))
                }
                None => Ok(report.to_json()),
            }
        }
        "latency-table" if !rest.is_empty() => {
            let reports = rest
                .iter()
                .map(|path| {
                    let text = std::fs::read_to_string(path)
                        .map_err(|error| BenchError::io(format!("reading {path}"), error))?;
                    LatencyReport::from_json(&text)
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok(match &reports[..] {
                [one] => latency::table(one),
                several => latency::runs_table(several),
            })
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
                    "--set" => settings.engine.set(value)?,
                    "--seed" => settings.seed = parse_number(flag, value)?,
                    "--documents" => settings.documents = parse_number(flag, value)?,
                    "--owners" => settings.owners = parse_number(flag, value)?,
                    "--churn" => settings.churn = parse_number(flag, value)?,
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
    options.check_engine_settings()?;
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

/// What a table says under its first line where no row of it has a count
/// of instructions, so that the empty column is not read as a machine
/// without a counter: for a report that says its run counted none, and
/// for one that says `perf` was counting.
const NOT_COUNTED: &str =
    "instr/sample is empty: the run counted no instructions (`run --perf` counts them)";
const NOT_GIVEN: &str =
    "instr/sample is empty: the run asked for instructions (--perf) and `perf` gave no count";

fn no_counts(report: &Report) -> Option<&'static str> {
    let mut rows = report.engines.iter().flat_map(|engine| &engine.rows);
    if rows.any(|row| row.instructions_per_sample.is_some()) {
        return None;
    }
    Some(match report.method.perf_instructions {
        false => NOT_COUNTED,
        true => NOT_GIVEN,
    })
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
    if let Some(no_counts) = no_counts(report) {
        let _ = writeln!(out, "{no_counts}");
    }
    if !report.method.engine_settings.is_empty() {
        let _ = writeln!(out, "settings: {}", report.method.engine_settings.join(" "));
    }
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
    let mut within = [0usize; 4];
    let mut total = [0usize; 4];
    let verdict = |agrees: bool| if agrees { "ok" } else { "APART" };
    for agreement in &agreements {
        let slot = match agreement.kind {
            RowKind::Count => 0,
            RowKind::Measured => 1,
            RowKind::Time => 2,
        };
        total[slot] += 1;
        let agrees = agreement.within(tolerance);
        within[slot] += usize::from(agrees);
        let instructions = agreement.instructions_within(tolerance);
        if let Some(agrees) = instructions {
            total[3] += 1;
            within[3] += usize::from(agrees);
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
        "within {}: {} of {} count rows, {} of {} measured rows, {} of {} time rows, \
         {} of {} time rows by instructions",
        percent(ratio(tolerance, 1.0)),
        within[0],
        total[0],
        within[1],
        total[1],
        within[2],
        total[2],
        within[3],
        total[3]
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
             --seed 7 --documents 2000 --owners 20 --churn 100 --creates 40 --updates 60 \
             --deletes 80 \
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
                settings.churn,
                settings.measured_creates,
                settings.updates,
                settings.deletes
            ),
            (7, 2000, 20, 100, 40, 60, 80)
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

    #[cfg(grovedb_engine_seam)]
    #[test]
    fn engines_are_measured_in_the_order_they_are_named() {
        let (options, _) = parse_run(&args("--engine rocksdb --engine lignindb")).unwrap();
        assert_eq!(
            options.engines,
            vec![EngineChoice::RocksDb, EngineChoice::LigninDb]
        );
        let (alone, _) = parse_run(&args("--engine lignindb")).unwrap();
        assert_eq!(alone.engines, vec![EngineChoice::LigninDb]);
        assert!(alone.settings.engine.is_empty());
    }

    #[cfg(grovedb_engine_seam)]
    #[test]
    fn settings_parse_for_a_run_that_measures_lignindb() {
        let (options, _) = parse_run(&args(
            "--engine rocksdb --engine lignindb --set chain_deltas=4 --set pace=block",
        ))
        .unwrap();
        assert_eq!(
            options.settings.engine.list(),
            ["chain_deltas=4", "pace=block"]
        );
        let method = options
            .settings
            .method(crate::bench::ColdMethod::Reopen, false);
        assert_eq!(method.engine_settings, ["chain_deltas=4", "pace=block"]);
        for (line, message) in [
            (
                "--engine lignindb --set pace",
                "--set takes KEY=VALUE, not \"pace\"",
            ),
            (
                "--engine lignindb --set colour=red",
                "LigninDB has no setting \"colour\"",
            ),
            ("--engine lignindb --set", "--set needs a value"),
            (
                "--set pace=block",
                "--set: no engine of this run takes a setting",
            ),
            (
                "--engine rocksdb --set pace=block",
                "--set: no engine of this run takes a setting",
            ),
        ] {
            assert_eq!(parse_run(&args(line)).unwrap_err().to_string(), message);
        }
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
        for line in [
            "",
            "fly",
            "table",
            "score",
            "compare a",
            "compare a b --tolerance",
        ] {
            assert_eq!(main_with(&args(line)).unwrap_err().to_string(), USAGE);
        }
        assert!(main_with(&args("table /nonexistent/report.json"))
            .unwrap_err()
            .to_string()
            .starts_with("reading /nonexistent/report.json"));
        assert!(main_with(&args("score /nonexistent/report.json"))
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
    fn the_latency_commands_make_a_report_and_table_it() {
        let dir = tempfile::tempdir().unwrap();
        let (scratch, out) = (dir.path().join("scratch"), dir.path().join("r.json"));
        let path = |path: &std::path::Path| path.display().to_string();
        let mut line = args(
            "latency --engine rocksdb --label head --documents 400 --owners 4 --warm-blocks 2 \
             --blocks 4 --pause-us 200 --scratch",
        );
        line.push(path(&scratch));
        // Without `--out` the command prints the report itself.
        let json = main_with(&line).unwrap();
        let report = LatencyReport::from_json(&json).unwrap();
        assert_eq!(report.engines[0].commit_ns.len(), 4);
        // With it the report is written there and its table is printed.
        line.extend(["--out".to_string(), path(&out)]);
        let text = main_with(&line).unwrap();
        assert!(text.contains("| head | rocksdb | default | 4 |"), "{text}");
        let written = std::fs::read_to_string(&out).unwrap();
        assert_eq!(LatencyReport::from_json(&written).unwrap().engines.len(), 1);
        // One report is tabled as the run printed it, and two of one run
        // as a row each and a row of both.
        let table = |reports: &[&std::path::Path]| {
            let mut line = args("latency-table");
            line.extend(reports.iter().map(|report| path(report)));
            main_with(&line)
        };
        assert_eq!(table(&[&out]).unwrap(), text);
        let both = table(&[&out, &out]).unwrap();
        assert!(both.contains("| rocksdb | default | 2 |"), "{both}");
        // No report, and a file that is none, are refused.
        assert_eq!(table(&[]).unwrap_err().to_string(), USAGE);
        let refused = table(&[&dir.path().join("none.json")]).unwrap_err();
        assert!(refused.to_string().contains("reading"), "{refused}");
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
