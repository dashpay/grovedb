//! The `lignin-bench` commands of the operator documents
//! (`docs/lignindb/operator`), held to the benchmark's own parser: each is
//! `lignin-bench run`, and `cli::parse_run` takes its options as written,
//! without running the workload. The rehearsal of step 3 of `migration.md`
//! keeps the RocksDB database it makes, which the steps after it migrate.
//!
//! `lignin-tools/tests/operator_docs.rs` holds the documents' other
//! commands to the code. `runbook.sh` runs the blocks of the documents and
//! keeps no command of the benchmark of its own.

use std::{fs, path::PathBuf};

use lignin_testkit::bench::{cli::parse_run, BenchError, EngineChoice, RunOptions};

/// The operator documents.
const DOCUMENTS: [&str; 5] = [
    "architecture.md",
    "migration.md",
    "backup-and-full-disk.md",
    "messages.md",
    "tuning.md",
];

fn operator_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../docs/lignindb/operator")
}

/// The simple commands of `text` that run `lignin-bench`, by the document
/// line they begin on: those of the blocks marked `sh`, with the lines a
/// `\` continues joined, and the code spans of the prose that name a
/// subcommand.
fn bench_commands(text: &str) -> Vec<(usize, String)> {
    let mut found = Vec::new();
    let mut fence: Option<String> = None;
    let mut pending: Option<(usize, String)> = None;
    for (at, line) in text.lines().enumerate().map(|(at, line)| (at + 1, line)) {
        let marker = line.trim_start();
        if let Some(info) = marker.strip_prefix("```") {
            fence = match fence {
                None => Some(info.trim().to_owned()),
                Some(_) => None,
            };
            continue;
        }
        match fence.as_deref() {
            Some("sh") => {
                let (start, mut joined) = pending.take().unwrap_or((at, String::new()));
                joined.push_str(line);
                if let Some(open) = joined.strip_suffix('\\') {
                    pending = Some((start, format!("{open} ")));
                    continue;
                }
                let commands = joined.split(['|', ';', '&']).map(str::trim);
                let commands = commands.filter(|command| command.starts_with("lignin-bench"));
                found.extend(commands.map(|command| (start, command.to_owned())));
            }
            Some(_) => {}
            None => {
                let spans = line.split('`').skip(1).step_by(2);
                let spans = spans.filter(|span| span.split_whitespace().count() > 1);
                let spans = spans.filter(|span| span.starts_with("lignin-bench "));
                found.extend(spans.map(|span| (at, span.to_owned())));
            }
        }
    }
    found
}

/// The words of `command` as the shell gives them to the program: up to
/// its first redirection, with the double quotes around a word taken off.
fn words(command: &str) -> Vec<String> {
    (command.split_whitespace())
        .take_while(|word| !word.starts_with(['>', '<']))
        .map(|word| word.trim_matches('"').to_owned())
        .collect()
}

/// What `lignin-bench` makes of `command`: `run` and the options of
/// [`parse_run`].
fn parsed(command: &str) -> Result<RunOptions, String> {
    let words = words(command);
    match words.get(1).map(String::as_str) {
        Some("run") => parse_run(&words[2..])
            .map(|(options, _)| options)
            .map_err(|error| error.to_string()),
        _ => Err("lignin-bench is run here with `run` alone".to_owned()),
    }
}

#[test]
fn every_command_of_the_benchmark_parses_as_written() {
    let mut read = 0;
    let mut faults = Vec::new();
    for name in DOCUMENTS {
        let text = fs::read_to_string(operator_dir().join(name)).expect("the document");
        for (line, command) in bench_commands(&text) {
            read += 1;
            if let Err(fault) = parsed(&command) {
                faults.push(format!("{name}:{line}: `{command}`: {fault}"));
            }
        }
    }
    assert!(faults.is_empty(), "{}", faults.join("\n"));
    assert!(read >= 2, "only {read} commands of lignin-bench were read");
    let script = fs::read_to_string(operator_dir().join("runbook.sh")).expect("runbook.sh");
    assert!(
        !script.contains("lignin-bench "),
        "runbook.sh runs a command of the benchmark of its own"
    );
}

/// The rehearsal of step 3 of `migration.md`: the one block's command of
/// the benchmark, which keeps the RocksDB database it makes under its
/// scratch directory, and only that engine's.
fn rehearsal() -> String {
    let text = fs::read_to_string(operator_dir().join("migration.md")).expect("migration.md");
    let mut found = bench_commands(&text).into_iter();
    let found = found.find(|(_, command)| command.contains("--scratch"));
    found.expect("the rehearsal in migration.md").1
}

#[test]
fn the_rehearsal_keeps_the_rocksdb_database_it_makes() {
    let command = rehearsal();
    let options = parsed(&command).unwrap_or_else(|fault| panic!("`{command}`: {fault}"));
    assert!(options.settings.keep, "`{command}` keeps no database");
    assert_eq!(options.engines, [EngineChoice::RocksDb], "`{command}`");
    assert_eq!(
        options.settings.scratch,
        PathBuf::from("$WORK/rehearsal"),
        "`{command}`"
    );
}

#[test]
fn the_parser_refuses_what_the_rehearsal_must_not_say() {
    let command = rehearsal();
    let documents = words(&command)
        .windows(2)
        .find(|pair| pair[0] == "--documents")
        .map(|pair| format!("--documents {}", pair[1]))
        .expect("the rehearsal's --documents");
    for (case, changed, fault) in [
        (
            "a count in words",
            command.replacen(&documents, "--documents twenty", 1),
            "--documents takes a number, not \"twenty\"",
        ),
        (
            "an unknown option",
            command.replacen(" --keep ", " --keep --no-such-option 1 ", 1),
            "unknown option --no-such-option",
        ),
        (
            "an option without its value",
            format!(
                "{} --owners",
                command.split(" >").next().expect("the command")
            ),
            "--owners needs a value",
        ),
        (
            "another subcommand",
            command.replacen("lignin-bench run ", "lignin-bench table ", 1),
            "lignin-bench is run here with `run` alone",
        ),
    ] {
        assert_ne!(changed, command, "{case}: the rehearsal changed");
        assert_eq!(parsed(&changed).err().as_deref(), Some(fault), "{case}");
    }
    let check = parse_run(&["--documents".to_owned(), "0".to_owned()]);
    assert!(
        matches!(check, Err(BenchError::Check(words)) if words == "the run needs documents and owners"),
        "the settings are checked"
    );
}
