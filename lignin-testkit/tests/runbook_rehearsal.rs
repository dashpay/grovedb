//! The `lignin-bench` commands of the operator documents
//! (`docs/lignindb/operator`), held to the benchmark's own parser: each is
//! `lignin-bench run`, and `cli::parse_run` takes its options as written,
//! without running the workload. The rehearsal of step 3 of `migration.md`
//! keeps the RocksDB database it makes, which the steps after it migrate.
//!
//! `lignin-tools/tests/operator_docs.rs` holds the documents' other
//! commands to the code, and reads a document as this test does
//! (`document_text`). `runbook.sh` runs the blocks of the documents and
//! runs no command of the benchmark of its own.

use std::{fs, path::PathBuf};

use lignin_testkit::bench::{cli::parse_run, BenchError, EngineChoice, RunOptions};

#[path = "../../lignin-tools/tests/document_text/mod.rs"]
mod document_text;

use document_text::{code_of, code_spans, commands, parse, simple_commands, words};

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

fn read(name: &str) -> String {
    fs::read_to_string(operator_dir().join(name)).expect("the document")
}

/// A command of the benchmark in a document.
struct Found {
    /// The line it is on, or that its block begins on.
    line: usize,
    /// Whether a block marked `sh` holds it, which `runbook.sh` runs.
    run: bool,
    /// The words the shell gives the program.
    words: Vec<String>,
}

/// Whether `words` run `lignin-bench`.
fn is_bench(words: &[String]) -> bool {
    words.first().is_some_and(|word| word == "lignin-bench")
}

/// The commands of `text` that run `lignin-bench`: the simple commands of
/// its blocks marked `sh`, and the code spans of its prose that name a
/// subcommand.
fn bench_commands(text: &str) -> Vec<Found> {
    let parsed = parse(text);
    let mut found = Vec::new();
    for fence in parsed.fences.iter().filter(|fence| fence.info == "sh") {
        let lines = commands(fence);
        let simple = lines.iter().flat_map(|line| simple_commands(line));
        let blocks = simple
            .map(|command| words(&command))
            .filter(|w| is_bench(w));
        found.extend(blocks.map(|words| Found {
            line: fence.line,
            run: true,
            words,
        }));
    }
    for (line, prose) in &parsed.prose {
        let spans = code_spans(prose).into_iter().map(|span| words(&span));
        let spans = spans.filter(|words| is_bench(words) && words.len() > 1);
        found.extend(spans.map(|words| Found {
            line: *line,
            run: false,
            words,
        }));
    }
    found
}

/// What `lignin-bench` makes of `words`: `run` and the options of
/// [`parse_run`].
fn parsed(words: &[String]) -> Result<RunOptions, String> {
    match words.get(1).map(String::as_str) {
        Some("run") => parse_run(&words[2..])
            .map(|(options, _)| options)
            .map_err(|error| error.to_string()),
        _ => Err("lignin-bench is run here with `run` alone".to_owned()),
    }
}

#[test]
fn every_command_of_the_benchmark_parses_as_written() {
    let mut read_commands = 0;
    let mut faults = Vec::new();
    for name in DOCUMENTS {
        for found in bench_commands(&read(name)) {
            read_commands += 1;
            if let Err(fault) = parsed(&found.words) {
                let command = found.words.join(" ");
                faults.push(format!("{name}:{}: `{command}`: {fault}", found.line));
            }
        }
    }
    assert!(faults.is_empty(), "{}", faults.join("\n"));
    assert!(
        read_commands >= 2,
        "only {read_commands} commands of lignin-bench were read"
    );
    let script = read("runbook.sh");
    let own: Vec<&str> = (script.lines())
        .filter(|line| code_of(line).contains("lignin-bench"))
        .collect();
    assert!(own.is_empty(), "runbook.sh names the benchmark: {own:?}");
}

/// The commands of the benchmark are read where a block puts them in a
/// condition, after `&&`, after a comment that ends in `\`, and where a
/// span is set off by two backticks; not in a comment; and with the words
/// after a redirection.
#[test]
fn a_command_of_the_benchmark_is_read_in_every_place_it_can_be() {
    let text = "Prose with ``lignin-bench run --in-a-span 1`` in it.\n\n\
                ```sh\n\
                if lignin-bench run --in-a-condition 1; then :; fi\n\
                true && lignin-bench run --after-and 1\n\
                true # ignored; lignin-bench run --in-a-comment 1\n\
                # a comment that ends in a backslash \\\n\
                lignin-bench run --after-a-comment 1\n\
                ```\n";
    let faults: Vec<(bool, String)> = (bench_commands(text).iter())
        .map(|found| (found.run, parsed(&found.words).err().unwrap_or_default()))
        .collect();
    assert_eq!(
        faults,
        [
            (true, "unknown option --in-a-condition".to_owned()),
            (true, "unknown option --after-and".to_owned()),
            (true, "unknown option --after-a-comment".to_owned()),
            (false, "unknown option --in-a-span".to_owned()),
        ]
    );
    for line in [
        "lignin-bench run --keep > out --after-a-redirection 2",
        "lignin-bench run --keep >> out --after-a-redirection 2",
        "lignin-bench run --keep &> out --after-a-redirection 2",
        "lignin-bench run --keep &>> out --after-a-redirection 2",
        "lignin-bench run --keep 2>&1 > out --after-a-redirection 2",
        "lignin-bench run --keep < in --after-a-redirection 2",
        "lignin-bench run --reopens 1&>out --after-a-redirection 2",
    ] {
        let found = bench_commands(&format!("```sh\n{line}\n```\n"));
        let faults: Vec<_> = (found.iter())
            .map(|found| (found.run, parsed(&found.words).err()))
            .collect();
        let fault = "unknown option --after-a-redirection".to_owned();
        assert_eq!(faults, [(true, Some(fault))], "{line}");
    }
}

/// The rehearsal of step 3 of `migration.md`: the one command of the
/// benchmark of its blocks that names a scratch directory.
fn rehearsal() -> Vec<String> {
    let found = bench_commands(&read("migration.md"));
    let mut run = found.into_iter().filter(|found| found.run);
    let scratch = |found: &Found| found.words.iter().any(|word| word == "--scratch");
    let mut rehearsals = run.by_ref().filter(scratch);
    let rehearsal = rehearsals.next().expect("the rehearsal in migration.md");
    assert!(rehearsals.next().is_none(), "one rehearsal in migration.md");
    rehearsal.words
}

#[test]
fn the_rehearsal_keeps_the_rocksdb_database_it_makes() {
    let words = rehearsal();
    let options = parsed(&words).unwrap_or_else(|fault| panic!("{words:?}: {fault}"));
    assert!(options.settings.keep, "{words:?} keeps no database");
    assert_eq!(options.engines, [EngineChoice::RocksDb], "{words:?}");
    assert_eq!(
        options.settings.scratch,
        PathBuf::from("$WORK/rehearsal"),
        "{words:?}"
    );
}

#[test]
fn the_parser_refuses_what_the_rehearsal_must_not_say() {
    let rehearsal = rehearsal();
    let changed = |change: &dyn Fn(&mut Vec<String>)| {
        let mut words = rehearsal.clone();
        change(&mut words);
        words
    };
    let documents = (rehearsal.iter())
        .position(|word| word == "--documents")
        .expect("the rehearsal's --documents");
    let push = |more: &[&str]| changed(&|words| words.extend(more.iter().map(|w| w.to_string())));
    for (case, words, fault) in [
        (
            "a count in words",
            changed(&|words| words[documents + 1] = "twenty".to_owned()),
            "--documents takes a number, not \"twenty\"",
        ),
        (
            "an unknown option",
            push(&["--no-such-option", "1"]),
            "unknown option --no-such-option",
        ),
        (
            "an option without its value",
            push(&["--owners"]),
            "--owners needs a value",
        ),
        (
            "another subcommand",
            changed(&|words| words[1] = "table".to_owned()),
            "lignin-bench is run here with `run` alone",
        ),
    ] {
        assert_eq!(parsed(&words).err().as_deref(), Some(fault), "{case}");
    }
    let check = parse_run(&["--documents".to_owned(), "0".to_owned()]);
    assert!(
        matches!(check, Err(BenchError::Check(words)) if words == "the run needs documents and owners"),
        "the settings are checked"
    );
}
