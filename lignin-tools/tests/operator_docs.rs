//! The operator documents of `docs/lignindb/operator` (OPS-05) held to the
//! code they speak of:
//!
//! - every `lignin` command in them parses, and every option they name is
//!   one the subcommand takes;
//! - every `cargo` line names a package, a feature and a program the
//!   workspace has, and every `lignin-bench` line options that its parser
//!   reads;
//! - every message they quote is in the code: in the file that the
//!   messages chapter names for it, and anywhere in the engine's crates for
//!   a message quoted elsewhere;
//! - the defaults of the tuning chapter and of the runbook are the code's;
//! - the comparisons of steps 7 and 11 of the runbook answer as it says,
//!   run on reports made for each case, and steps 4 and 11 refuse a
//!   directory that holds an entry of LigninDB;
//! - the chapters of the book are the documents.
//!
//! `docs/lignindb/operator/runbook.sh` runs the commands themselves.
//! `LIGNIN_OPERATOR_DOCS=write cargo test -p lignin-tools --test
//! operator_docs` writes the book's chapters from the documents.

use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    fs,
    path::{Component, Path, PathBuf},
    time::Duration,
};

use lignin_format::PageSize;
use lignin_store::{
    cleaner::CleanerConfig,
    flush::FlushConfig,
    wal::{LogOptions, SyncPolicy},
};
use lignin_tools::args::{
    self, Unreachable, DEFAULT_CACHE, DEFAULT_COMPACT_TIMEOUT, DEFAULT_MIGRATE_BATCH,
    DEFAULT_MIGRATE_PROGRESS,
};
use lignindb::Options;

/// The documents, in the order the book has them.
const CHAPTERS: [&str; 5] = [
    "architecture.md",
    "migration.md",
    "backup-and-full-disk.md",
    "messages.md",
    "tuning.md",
];

/// Where a document links to a file of the repository that is no chapter,
/// the book links to it on the branch the book is published from.
const PUBLISHED: &str = "https://github.com/dashpay/grovedb/blob/develop/";

/// The crates whose words a message quoted outside the messages chapter
/// can be.
const SPEAKERS: [&str; 11] = [
    "lignindb/src",
    "merk/src/store/lignin",
    "lignin-store/src",
    "lignin-tree/src",
    "lignin-format/src",
    "lignin-io/src",
    "lignin-tools/src",
    "grovedb/src/engine",
    "grovedb/src/error.rs",
    "storage/src/error.rs",
    "lignin-testkit/src/bench",
];

/// The error types of an open, a recovery, a commit, the end of a
/// session, a close, a checkpoint and the tool, by the file their
/// `Display` is in: the messages chapter quotes every message of each.
const COVERED: [(&str, &[&str]); 26] = [
    ("lignin-tree/src/drop/error.rs", &["DropError"]),
    ("lignin-tree/src/flat/error.rs", &["FlatError"]),
    ("lignin-tree/src/directory/error.rs", &["DirectoryError"]),
    ("lignin-format/src/error.rs", &["FormatError"]),
    ("lignin-format/src/manifest.rs", &["ManifestError"]),
    ("lignin-format/src/wal.rs", &["WalError"]),
    ("lignin-format/src/tree_page.rs", &["TreePageError"]),
    ("lignin-format/src/table.rs", &["TableError"]),
    ("lignin-format/src/value_log.rs", &["ValueLogError"]),
    ("lignin-format/src/flat_page.rs", &["FlatPageError"]),
    ("lignin-format/src/segment.rs", &["SegmentError"]),
    ("lignin-format/src/delta.rs", &["DeltaError"]),
    (
        "lignin-store/src/table/error.rs",
        &["LinkError", "PageTableError"],
    ),
    (
        "lignin-store/src/values/error.rs",
        &["Miscount", "ValueStoreError"],
    ),
    ("lignin-store/src/load/error.rs", &["LoadError"]),
    (
        "lignin-store/src/alloc/error.rs",
        &["AllocError", "RecoveryError"],
    ),
    (
        "lignin-store/src/cache/error.rs",
        &["CacheError", "StepRefusal"],
    ),
    ("lignin-store/src/flush/error.rs", &["FlushError"]),
    ("lignin-store/src/cleaner/error.rs", &["CleanerError"]),
    (
        "lignin-store/src/segment/error.rs",
        &["SegmentStoreError", "ManifestChange"],
    ),
    ("lignin-store/src/wal/error.rs", &["LogError"]),
    (
        "lignindb/src/db/error.rs",
        &["OpenError", "Reopen", "DbError"],
    ),
    (
        "lignindb/src/commit/error.rs",
        &["Stopped", "CommitError", "Failure"],
    ),
    ("lignin-store/src/recovery/error.rs", &["RecoveryError"]),
    ("lignindb/src/checkpoint/error.rs", &["CheckpointError"]),
    ("lignin-tools/src/error.rs", &["ToolError"]),
];

/// Words that the code hands to a message of those types as a value, by
/// the file, the words before each, and the message they fill: the string
/// literal after each, or every literal of the call where the words open
/// one, is quoted as the message of a row of the messages chapter that
/// names that file and quotes that message too (where one is named: an
/// empty one asks for the row alone), so a reason quoted for one
/// message does not stand for the same word of another.
const REASONS: [(&str, &str, &str); 46] = [
    ("lignin-io/src/linux/ring.rs", "wipe_on_fork().map_err(", ""),
    (
        "lignin-io/src/linux/ring.rs",
        ".ok_or_else(|| io::Error::other(",
        "",
    ),
    ("lignin-io/src/std_fs.rs", "io::Error::other(", ""),
    ("lignin-io/src/std_fs.rs", "format!(", ""),
    ("lignin-io/src/std_fs.rs", "path_error(", ""),
    ("lignin-io/src/std_fs.rs", "wrong_mode(", ""),
    ("lignin-io/src/linux.rs", "format!(", ""),
    ("lignin-io/src/linux.rs", "io::Error::other(", ""),
    ("lignin-io/src/linux.rs", "io::Error::new(", ""),
    ("lignin-io/src/linux/aligned.rs", "io::Error::other(", ""),
    (
        "lignin-io/src/linux.rs",
        "no_direct_io(",
        "… cannot be opened for direct reads: …",
    ),
    (
        "lignin-tree/src/directory/check.rs",
        "damaged(",
        DIRECTORY_RECORD,
    ),
    (
        "lignin-tree/src/directory/check.rs",
        "let what = ",
        DIRECTORY_RECORD,
    ),
    (
        "lignin-tree/src/directory/rekey.rs",
        "let what = ",
        DIRECTORY_RECORD,
    ),
    (
        "lignin-tree/src/directory/rekey.rs",
        "let twice = ",
        DIRECTORY_RECORD,
    ),
    (
        "lignin-tree/src/directory/write.rs",
        "let what = ",
        DIRECTORY_RECORD,
    ),
    (
        "lignin-tree/src/directory/read.rs",
        "Malformed(",
        DIRECTORY_RECORD,
    ),
    (
        "lignin-tree/src/directory/record.rs",
        "Malformed(",
        DIRECTORY_RECORD,
    ),
    (
        "lignin-tree/src/directory/record.rs",
        "let what = ",
        DIRECTORY_RECORD,
    ),
    ("lignin-tree/src/flat/cursor.rs", "what: ", FLAT_PAGE),
    ("lignin-tree/src/flat/long_key.rs", "what: ", FLAT_PAGE),
    ("lignin-tree/src/flat/node.rs", "what: ", FLAT_PAGE),
    ("lignin-tree/src/flat/store.rs", "what: ", FLAT_PAGE),
    ("lignin-tree/src/flat/tree.rs", "what: ", FLAT_PAGE),
    ("lignin-tree/src/drop/step.rs", "damaged(", DROP_PAGE),
    ("lignin-tree/src/drop/flat.rs", "damaged(", DROP_PAGE),
    ("lignin-tree/src/drop/flat.rs", "let twice = ", DROP_PAGE),
    ("lignin-tree/src/drop/column.rs", "damaged(", DROP_PAGE),
    ("lignin-tree/src/drop/column.rs", "let what = ", DROP_PAGE),
    ("lignin-tree/src/drop/column.rs", ": &str = ", DROP_PAGE),
    ("lignin-tree/src/drop/merk.rs", "let what = ", DROP_PAGE),
    ("lignin-tree/src/drop/step.rs", "Record(", DROP_RECORD),
    ("lignin-tree/src/drop/flat.rs", "Record(", DROP_RECORD),
    ("lignin-tree/src/drop/column.rs", "Record(", DROP_RECORD),
    ("lignindb/src/db/open.rs", "tree(", FIRST_COMMIT),
    ("lignindb/src/db/open.rs", "column(", FIRST_COMMIT),
    (
        "lignindb/src/db/mod.rs",
        "let ended = ",
        "cannot start the background work: ",
    ),
    (
        "lignindb/src/checkpoint/files.rs",
        "CheckpointError::io(",
        "could not … …: …",
    ),
    (
        "lignindb/src/checkpoint/create.rs",
        "reason: ",
        "log file … …",
    ),
    (
        "lignindb/src/db/open.rs",
        "OpenError::create(",
        FIRST_COMMIT,
    ),
    ("lignindb/src/db/open.rs", "OpenError::catalog(", KEYSPACES),
    ("lignindb/src/db/open.rs", "OpenError::io(", "cannot … …: …"),
    ("lignindb/src/db/open.rs", "what: ", KEYSPACES),
    ("lignindb/src/db/open.rs", "let what = ", KEYSPACES),
    ("lignindb/src/db/open.rs", "read(key::LAYOUT, ", KEYSPACES),
    ("lignindb/src/db/mod.rs", "OpenError::catalog(", KEYSPACES),
];
/// The functions of GroveDB that an open of a directory (with its
/// integrity check's walk of the elements, its opens of Merks and the
/// decoding of an element) and the deletion of a checkpoint run through around the
/// engine, by file and the words their definition begins with: every message they build
/// (`Error::…(…)`) is quoted in a row that names the file. Errors that
/// answer an application's misuse of the API are elsewhere, and left out.
const GROVEDB_FUNCTIONS: [(&str, &str); 12] = [
    (
        "grovedb/src/lib.rs",
        "pub fn open_with_cidx_integrity_check<",
    ),
    ("grovedb/src/lib.rs", "fn verify_elements_up_to_a_tree<"),
    ("grovedb/src/lib.rs", "fn open_transactional_merk_at_path<"),
    ("grovedb/src/util/compat.rs", "pub(crate) fn open_merk<"),
    (
        "grovedb-element/src/element/serialize.rs",
        "pub fn deserialize(",
    ),
    ("grovedb/src/engine/either.rs", "fn held_among("),
    ("grovedb/src/engine/either.rs", "fn unlisted("),
    ("grovedb/src/engine/either.rs", "fn refuse_a_reserved_name("),
    ("grovedb/src/engine/either.rs", "fn no_checkpoint("),
    ("grovedb/src/engine/either.rs", "fn refuse_the_other("),
    (
        "grovedb/src/engine/either.rs",
        "pub fn delete_checkpoint_of<",
    ),
    (
        "grovedb/src/checkpoints.rs",
        "pub fn delete_checkpoint_with_engine<",
    ),
];
const FIRST_COMMIT: &str = "the first commit of the database (";
const KEYSPACES: &str = "the record of the keyspaces: ";
const FLAT_PAGE: &str = "flat page …: …";
const DIRECTORY_RECORD: &str = "subtree directory record ";
const DROP_PAGE: &str = "what a dropped subtree left in page …: …";
const DROP_RECORD: &str = "record of unlinked pages: …";

/// Words of those types that fill a place of a longer message of theirs,
/// which the chapter quotes with `…` there.
const PARTS: [&str; 2] = ["up to …", "offset … of log file …"];

/// The first words of the commands a document can name in a code span.
const COMMANDS: [&str; 22] = [
    "exec",
    "chown",
    "lignin",
    "lignin-bench",
    "cargo",
    "git",
    "cp",
    "rm",
    "sync",
    "find",
    "touch",
    "df",
    "mkdir",
    "set",
    "tee",
    "grep",
    "curl",
    "sh",
    "bash",
    "rustup",
    "pwd",
    "cd",
];

fn repository() -> PathBuf {
    let tools = Path::new(env!("CARGO_MANIFEST_DIR"));
    tools.parent().expect("the workspace").to_path_buf()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn operator_dir() -> PathBuf {
    repository().join("docs/lignindb/operator")
}

/// Every document, by its name.
fn documents() -> Vec<(&'static str, String)> {
    let dir = operator_dir();
    CHAPTERS
        .iter()
        .map(|name| (*name, read(&dir.join(name))))
        .collect()
}

/// A fenced block of a document.
struct Fence {
    info: String,
    line: usize,
    body: Vec<String>,
}

/// What a document holds: its fenced blocks, and its other lines with
/// their numbers.
struct Parsed {
    fences: Vec<Fence>,
    prose: Vec<(usize, String)>,
}

fn parse(text: &str) -> Parsed {
    let mut parsed = Parsed {
        fences: Vec::new(),
        prose: Vec::new(),
    };
    let mut open: Option<(usize, Fence)> = None;
    for (at, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        match open.as_mut() {
            None => match trimmed.strip_prefix("```") {
                Some(info) => {
                    let fence = Fence {
                        info: info.trim().to_owned(),
                        line: at + 1,
                        body: Vec::new(),
                    };
                    open = Some((indent, fence));
                }
                None => parsed.prose.push((at + 1, line.to_owned())),
            },
            Some((_, _)) if trimmed == "```" => {
                let (_, fence) = open.take().expect("open");
                parsed.fences.push(fence);
            }
            Some((fence_indent, fence)) => {
                let body = line.get(*fence_indent..).unwrap_or(trimmed);
                fence.body.push(body.to_owned());
            }
        }
    }
    assert!(open.is_none(), "a fence that is not closed");
    parsed
}

/// The code spans of a line of prose, as CommonMark reads them.
fn code_spans(line: &str) -> Vec<String> {
    spans_and_unclosed(line).0
}

/// The code spans of a line, and whether a run of backticks in it opens a
/// span that the line does not close: a span over two lines, which the
/// tests would not read whole.
fn spans_and_unclosed(line: &str) -> (Vec<String>, bool) {
    let chars: Vec<char> = line.chars().collect();
    let run = |from: usize| chars[from..].iter().take_while(|c| **c == '`').count();
    let mut spans = Vec::new();
    let mut unclosed = false;
    let mut at = 0;
    while at < chars.len() {
        if chars[at] == '\\' {
            at += 2;
            continue;
        }
        if chars[at] != '`' {
            at += 1;
            continue;
        }
        let opening = run(at);
        let start = at + opening;
        let mut end = start;
        let mut closed = None;
        while end < chars.len() {
            if chars[end] == '`' {
                let closing = run(end);
                if closing == opening {
                    closed = Some(end);
                    break;
                }
                end += closing;
            } else {
                end += 1;
            }
        }
        match closed {
            Some(end) => {
                let mut span: String = chars[start..end].iter().collect();
                let padded = span.len() >= 2 && span.starts_with(' ') && span.ends_with(' ');
                if padded && !span.trim().is_empty() {
                    span = span[1..span.len() - 1].to_owned();
                }
                spans.push(span);
                at = end + opening;
            }
            None => {
                unclosed = true;
                at = start;
            }
        }
    }
    (spans, unclosed)
}

/// The cells of a row of a table, or `None` for a line that is no row.
fn cells(line: &str) -> Option<Vec<String>> {
    let inner = line.trim().strip_prefix('|')?.strip_suffix('|')?;
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut chars = inner.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cell.push('|');
                chars.next();
            }
            '|' => cells.push(std::mem::take(&mut cell).trim().to_owned()),
            c => cell.push(c),
        }
    }
    cells.push(cell.trim().to_owned());
    Some(cells)
}

/// A table of a document: its header, and its rows with their line.
struct Table {
    header: Vec<String>,
    rows: Vec<(usize, Vec<String>)>,
}

fn tables(prose: &[(usize, String)]) -> Vec<Table> {
    let mut tables = Vec::new();
    let mut current: Option<Table> = None;
    let mut previous = 0;
    for (at, line) in prose {
        let row = cells(line).filter(|_| *at == previous + 1 || current.is_none());
        match (row, current.as_mut()) {
            (Some(row), None) => {
                current = Some(Table {
                    header: row,
                    rows: Vec::new(),
                })
            }
            (Some(row), Some(table)) => {
                let separator = row
                    .iter()
                    .all(|cell| cell.chars().all(|c| "-: ".contains(c)));
                if !separator {
                    table.rows.push((*at, row));
                }
            }
            (None, _) => tables.extend(current.take()),
        }
        if cells(line).is_none() {
            tables.extend(current.take());
        }
        previous = *at;
    }
    tables.extend(current);
    tables
}

/// The words of a command line as the shell splits them, up to the first
/// character that ends a command or begins a redirection.
fn words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                in_word = true;
                word.extend(chars.by_ref().take_while(|c| *c != '\''));
            }
            '"' => {
                in_word = true;
                let mut depth = 0;
                for c in chars.by_ref() {
                    match c {
                        '(' => depth += 1,
                        ')' => depth -= 1,
                        '"' if depth == 0 => break,
                        _ => {}
                    }
                    word.push(c);
                }
            }
            '>' | '<' if in_word && word.chars().all(|c| c.is_ascii_digit()) => {
                in_word = false;
                break;
            }
            '|' | ';' | '&' | '>' | '<' => break,
            c if c.is_whitespace() => {
                if in_word {
                    words.push(std::mem::take(&mut word));
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word {
        words.push(word);
    }
    words
}

/// The commands of a fenced block: its lines, with a line that ends in `\`
/// joined to the next.
fn commands(fence: &Fence) -> Vec<String> {
    let mut commands = Vec::new();
    let mut pending = String::new();
    for line in &fence.body {
        match line.strip_suffix('\\') {
            Some(start) => {
                pending.push_str(start);
                pending.push(' ');
            }
            None => {
                pending.push_str(line);
                commands.push(std::mem::take(&mut pending));
            }
        }
    }
    commands
}

fn os(words: &[&str]) -> Vec<OsString> {
    words.iter().map(OsString::from).collect()
}

/// A command line of each subcommand that parses as it is.
const FORMS: [&[&str]; 12] = [
    &["help"],
    &["manifest", "D"],
    &["segments", "D"],
    &["pages", "D"],
    &["directory", "D"],
    &["verify", "D"],
    &["stats", "D"],
    &["compact", "D"],
    &["dump", "D", "--all", "F"],
    &["load", "D", "F"],
    &["migrate", "S", "T"],
    &["migrate", "S"],
];

/// Whether `lignin` has the subcommand `name`: a form of it here, which
/// the tool's parser takes.
fn has_subcommand(name: &str) -> bool {
    FORMS
        .iter()
        .any(|form| form[0] == name && args::parse(&os(form)).is_ok())
}

/// Whether some command line of `subcommand` (of any, with none) takes
/// `option`, with a value or without.
fn takes(subcommand: Option<&str>, option: &str) -> bool {
    let forms = FORMS
        .iter()
        .filter(|form| subcommand.is_none_or(|name| form[0] == name));
    forms.into_iter().any(|form| {
        [None, Some("1"), Some(Unreachable::default().name())]
            .into_iter()
            .any(|value| {
                let mut line = form.to_vec();
                line.push(option);
                line.extend(value);
                args::parse(&os(&line)).is_ok()
            })
    })
}

/// What a manifest of the workspace names a package, by that name.
fn manifests() -> BTreeMap<String, (PathBuf, String)> {
    let mut found = BTreeMap::new();
    let root = repository();
    for entry in fs::read_dir(&root).expect("the workspace").flatten() {
        let manifest = entry.path().join("Cargo.toml");
        let Ok(text) = fs::read_to_string(&manifest) else {
            continue;
        };
        let name = text
            .lines()
            .skip_while(|line| line.trim() != "[package]")
            .find_map(|line| line.trim().strip_prefix("name = "))
            .map(|name| name.trim_matches('"').to_owned());
        if let Some(name) = name {
            found.insert(name, (entry.path(), text));
        }
    }
    found
}

/// Whether the manifest `text` declares `feature`.
fn declares_feature(text: &str, feature: &str) -> bool {
    let mut lines = text.lines().skip_while(|line| line.trim() != "[features]");
    lines.next();
    lines.take_while(|line| !line.starts_with('[')).any(|line| {
        line.split('=')
            .next()
            .is_some_and(|key| key.trim() == feature)
    })
}

/// Whether the package at `dir`, with the manifest `text`, has the program
/// `bin`.
fn has_program(dir: &Path, text: &str, bin: &str) -> bool {
    let named = format!("name = \"{bin}\"");
    let declared = text
        .split("[[bin]]")
        .skip(1)
        .any(|section| section.lines().any(|line| line.trim() == named));
    declared || dir.join("src/bin").join(format!("{bin}.rs")).is_file()
}

/// What is wrong with the command `line`, of a document, where it is one
/// that this test reads.
fn command_fault(line: &str, packages: &BTreeMap<String, (PathBuf, String)>) -> Option<String> {
    let words = words(line);
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    match words.first().copied() {
        Some("lignin") => args::parse(&os(&words[1..]))
            .err()
            .map(|error| format!("does not parse: {error}")),
        Some("lignin-bench") => {
            let cli = read(&repository().join("lignin-testkit/src/bench/cli.rs"));
            if words.get(1) != Some(&"run") {
                return Some("lignin-bench is run here with `run` alone".to_owned());
            }
            let body = function_body(&cli, "pub fn parse_run(");
            let unknown: Vec<&str> = (words[2..].iter().copied())
                .filter(|word| word.starts_with("--"))
                .filter(|word| !body.contains(&format!("\"{word}\"")))
                .collect();
            (!unknown.is_empty()).then(|| format!("lignin-bench run does not read {unknown:?}"))
        }
        Some("cargo") => {
            let value = |option: &str| {
                let at = words.iter().position(|word| *word == option)?;
                words.get(at + 1).copied()
            };
            let package = value("-p")?;
            let Some((dir, text)) = packages.get(package) else {
                return Some(format!("no package {package}"));
            };
            let features = value("--features")
                .into_iter()
                .flat_map(|list| list.split(','));
            for feature in features {
                if !declares_feature(text, feature) {
                    return Some(format!("{package} has no feature {feature}"));
                }
            }
            match value("--bin") {
                Some(bin) if !has_program(dir, text, bin) => {
                    Some(format!("{package} has no program {bin}"))
                }
                _ => None,
            }
        }
        _ => None,
    }
}

/// The text of the item that begins at `start` in `source`, up to the
/// brace that closes it.
fn function_body<'a>(source: &'a str, start: &str) -> &'a str {
    let at = source
        .find(start)
        .unwrap_or_else(|| panic!("no {start:?} in the source"));
    let mut depth = 0;
    for (offset, c) in source[at..].char_indices() {
        match c {
            '{' => depth += 1,
            '}' if depth == 1 => return &source[at..at + offset + 1],
            '}' => depth -= 1,
            _ => {}
        }
    }
    panic!("{start:?} does not end");
}

/// Whether a code span of prose is the text of a message, which is to be
/// found in the code: it has words, and is no command, no option and no
/// name of the code.
fn is_message(span: &str) -> bool {
    let first = span.split_whitespace().next().unwrap_or_default();
    span.contains(char::is_whitespace)
        && !COMMANDS.contains(&first)
        && !span.starts_with("--")
        && !span.contains("::")
}

/// `source` with every item that is built for tests alone (`#[cfg(test)]`,
/// `#[cfg(all(test, …))]`) left out: from its attribute to the `;` or `,`
/// that ends it, or to the brace that closes its body. Strings, characters
/// and comments are read as such, so a bracket in one counts for nothing.
fn outside_tests(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let starts = |at: usize, text: &str| {
        text.chars()
            .enumerate()
            .all(|(i, c)| chars.get(at + i) == Some(&c))
    };
    let mut out = String::with_capacity(source.len());
    let mut depth = 0usize;
    // Where an item for tests began, and whether its body has opened.
    let mut item: Option<(usize, bool)> = None;
    let mut at = 0;
    while at < chars.len() {
        let len = token_len(&chars, at);
        let c = chars[at];
        let mut keep = item.is_none();
        if len == 1 {
            match c {
                '#' if item.is_none()
                    && (starts(at, "#[cfg(test)]") || starts(at, "#[cfg(all(test,")) =>
                {
                    item = Some((depth, false));
                    keep = false;
                }
                '{' | '(' | '[' => {
                    depth += 1;
                    if let Some((began, opened)) = &mut item {
                        *opened |= c == '{' && depth == *began + 1;
                    }
                }
                '}' | ')' | ']' => {
                    depth = depth.saturating_sub(1);
                    match item {
                        // The bracket around the item closes: it ends, and
                        // the bracket is the code's.
                        Some((began, _)) if depth < began => {
                            item = None;
                            keep = true;
                        }
                        Some((began, true)) if c == '}' && depth == began => item = None,
                        _ => {}
                    }
                }
                ';' | ',' if item.is_some_and(|(began, opened)| depth == began && !opened) => {
                    item = None;
                }
                _ => {}
            }
        }
        if keep {
            out.extend(&chars[at..at + len]);
        }
        at += len;
    }
    out
}

/// How many characters the token at `at` takes: a comment, a string, a
/// character, or one character.
fn token_len(chars: &[char], at: usize) -> usize {
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let next = chars.get(at + 1).copied();
    let rest = chars.len() - at;
    match chars[at] {
        '/' if next == Some('/') => (chars[at..].iter().position(|c| *c == '\n')).unwrap_or(rest),
        '/' if next == Some('*') => {
            let (mut depth, mut end) = (0, at);
            while end < chars.len() {
                match (chars[end], chars.get(end + 1)) {
                    ('/', Some('*')) => {
                        depth += 1;
                        end += 2;
                    }
                    ('*', Some('/')) => {
                        depth -= 1;
                        end += 2;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => end += 1,
                }
            }
            end - at
        }
        '\'' => match (next, chars.get(at + 2)) {
            (Some('\\'), _) => {
                let close = chars[at + 2..].iter().position(|c| *c == '\'');
                close.map_or(1, |close| close + 3)
            }
            (Some(_), Some('\'')) => 3,
            _ => 1,
        },
        'r' if (at == 0 || !ident(chars[at - 1])) && matches!(next, Some('"') | Some('#')) => {
            let hashes = chars[at + 1..].iter().take_while(|c| **c == '#').count();
            if chars.get(at + 1 + hashes) != Some(&'"') {
                return 1;
            }
            let mut end = at + 2 + hashes;
            let closes = |end: usize| {
                chars[end] == '"'
                    && chars[end + 1..]
                        .iter()
                        .take(hashes)
                        .filter(|c| **c == '#')
                        .count()
                        == hashes
            };
            while end < chars.len() && !closes(end) {
                end += 1;
            }
            (end + 1 + hashes).min(chars.len()) - at
        }
        '"' => {
            let mut end = at + 1;
            while end < chars.len() && chars[end] != '"' {
                end += if chars[end] == '\\' { 2 } else { 1 };
            }
            (end + 1).min(chars.len()) - at
        }
        _ => 1,
    }
}

/// The arguments of a call, from just after its `(` to the `)` that closes
/// it, read as Rust.
fn call_arguments(rest: &str) -> String {
    let chars: Vec<char> = rest.chars().collect();
    let (mut depth, mut at) = (1, 0);
    while at < chars.len() {
        let len = token_len(&chars, at);
        if len == 1 {
            match chars[at] {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' | '}' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                }
                _ => {}
            }
        }
        at += len;
    }
    chars[..at].iter().collect()
}

/// A string literal of Rust source as a pattern of the text it formats:
/// escapes made, a line that ends in `\` joined to the next with no space
/// between, and every argument of a format string as `…`. The literals of
/// an item built for tests alone are not read ([`outside_tests`]).
fn literal_patterns(source: &str) -> Vec<String> {
    let chars: Vec<char> = outside_tests(source).chars().collect();
    let mut literals = Vec::new();
    let mut at = 0;
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    while at < chars.len() {
        let c = chars[at];
        let next = chars.get(at + 1).copied();
        match c {
            '/' if next == Some('/') => {
                while at < chars.len() && chars[at] != '\n' {
                    at += 1;
                }
            }
            '/' if next == Some('*') => {
                let mut depth = 0;
                while at < chars.len() {
                    match (chars[at], chars.get(at + 1)) {
                        ('/', Some('*')) => {
                            depth += 1;
                            at += 2;
                        }
                        ('*', Some('/')) => {
                            depth -= 1;
                            at += 2;
                            if depth == 0 {
                                break;
                            }
                        }
                        _ => at += 1,
                    }
                }
            }
            '\'' => {
                at += match (next, chars.get(at + 2)) {
                    (Some('\\'), _) => {
                        let close = chars[at + 2..].iter().position(|c| *c == '\'');
                        close.map_or(1, |close| close + 3)
                    }
                    (Some(_), Some('\'')) => 3,
                    _ => 1,
                };
            }
            'r' if (at == 0 || !ident(chars[at - 1])) && matches!(next, Some('"') | Some('#')) => {
                let hashes = chars[at + 1..].iter().take_while(|c| **c == '#').count();
                if chars.get(at + 1 + hashes) != Some(&'"') {
                    at += 1;
                    continue;
                }
                let start = at + 2 + hashes;
                let mut end = start;
                let closes = |end: usize| {
                    chars[end] == '"'
                        && chars[end + 1..]
                            .iter()
                            .take(hashes)
                            .filter(|c| **c == '#')
                            .count()
                            == hashes
                };
                while end < chars.len() && !closes(end) {
                    end += 1;
                }
                literals.push(chars[start..end].iter().collect::<String>());
                at = end + 1 + hashes;
            }
            '"' => {
                let mut text = String::new();
                at += 1;
                while at < chars.len() && chars[at] != '"' {
                    if chars[at] != '\\' {
                        text.push(chars[at]);
                        at += 1;
                        continue;
                    }
                    let escaped = chars.get(at + 1).copied().unwrap_or('\\');
                    at += 2;
                    match escaped {
                        'n' => text.push('\n'),
                        't' => text.push('\t'),
                        'r' => text.push('\r'),
                        '0' => text.push('\0'),
                        '\n' => {
                            while at < chars.len() && chars[at].is_whitespace() {
                                at += 1;
                            }
                        }
                        'x' => at += 2,
                        'u' => {
                            while at < chars.len() && chars[at] != '}' {
                                at += 1;
                            }
                            at += 1;
                        }
                        other => text.push(other),
                    }
                }
                at += 1;
                literals.push(text);
            }
            _ => at += 1,
        }
    }
    literals.iter().map(|literal| formatted(literal)).collect()
}

/// `literal` with each argument of a format string as `…`.
fn formatted(literal: &str) -> String {
    let mut out = String::new();
    let mut chars = literal.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, chars.peek()) {
            ('{', Some('{')) | ('}', Some('}')) => {
                out.push(c);
                chars.next();
            }
            ('{', _) => {
                chars.by_ref().take_while(|c| *c != '}').for_each(drop);
                out.push('…');
            }
            (c, _) => out.push(c),
        }
    }
    out
}

/// The literals of every Rust file at or below `path`, but for the code of
/// tests (a directory `tests`, a file `tests.rs`), whose expectations can
/// keep the words of a message that the code no longer has.
fn patterns_below(path: &Path) -> Vec<String> {
    let name = path.file_name().map(|name| name.to_string_lossy());
    if name.is_some_and(|name| name == "tests" || name == "tests.rs") {
        return Vec::new();
    }
    if path.is_file() {
        return literal_patterns(&read(path));
    }
    let mut found = Vec::new();
    let mut entries: Vec<PathBuf> = fs::read_dir(path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()))
        .flatten()
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    let tests = test_modules(path);
    for entry in entries {
        let module = entry
            .file_stem()
            .map(|stem| stem.to_string_lossy().into_owned());
        if module.is_some_and(|module| tests.contains(&module)) {
            continue;
        }
        if entry.is_dir() || entry.extension().is_some_and(|extension| extension == "rs") {
            found.extend(patterns_below(&entry));
        }
    }
    found
}

/// Whether the Rust file `file` is code of tests: it is in a directory
/// `tests` or is a file `tests.rs`, or it, or a directory above it, is a
/// module that its module file declares for tests alone.
fn is_test_code(file: &Path) -> bool {
    file.ancestors()
        .take_while(|path| !path.join("Cargo.toml").exists())
        .any(|path| {
            let (Some(dir), Some(stem)) = (path.parent(), path.file_stem()) else {
                return false;
            };
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned());
            name.is_some_and(|name| name == "tests" || name == "tests.rs")
                || test_modules(dir).contains(&*stem.to_string_lossy())
        })
}

/// The modules of the directory `dir` that its module file declares for
/// tests alone (`#[cfg(test)] mod name;`): their files are code of tests.
fn test_modules(dir: &Path) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let files = ["mod.rs", "lib.rs", "main.rs"].map(|name| dir.join(name));
    for file in files.into_iter().chain([dir.with_extension("rs")]) {
        let Ok(source) = fs::read_to_string(&file) else {
            continue;
        };
        for (at, _) in source.match_indices("#[cfg(") {
            let rest = &source[at..];
            if !rest.starts_with("#[cfg(test)]") && !rest.starts_with("#[cfg(all(test,") {
                continue;
            }
            // The item after this attribute and any others.
            let mut item = rest;
            while let Some(attribute) = item.trim_start().strip_prefix("#[") {
                item = attribute.split_once(']').map_or("", |(_, after)| after);
            }
            let item = item.trim_start();
            let item = ["pub(crate) ", "pub(super) ", "pub "]
                .iter()
                .find_map(|visibility| item.strip_prefix(visibility))
                .unwrap_or(item);
            let Some(declared) = item.strip_prefix("mod ") else {
                continue;
            };
            let name: String = declared
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            if declared[name.len()..].trim_start().starts_with(';') {
                names.insert(name);
            }
        }
    }
    names
}

/// The targets of the links of a line of prose.
fn link_targets(line: &str) -> Vec<&str> {
    let mut targets = Vec::new();
    let mut rest = line;
    while let Some(at) = rest.find("](") {
        let after = &rest[at + 2..];
        let Some(end) = after.find(')') else {
            break;
        };
        targets.push(&after[..end]);
        rest = &after[end..];
    }
    targets
}

/// The anchors GitHub and mdBook give the headings of a document.
fn anchors(text: &str) -> BTreeSet<String> {
    let parsed = parse(text);
    let headings = parsed
        .prose
        .iter()
        .filter_map(|(_, line)| line.strip_prefix('#'))
        .map(|heading| heading.trim_start_matches('#').trim());
    headings
        .map(|heading| {
            let lower = heading.to_lowercase();
            let kept = lower
                .chars()
                .filter(|c| c.is_alphanumeric() || matches!(c, ' ' | '-' | '_'));
            kept.map(|c| if c == ' ' { '-' } else { c }).collect()
        })
        .collect()
}

/// `relative`, a link of a document in `docs/lignindb/operator`, as a path
/// from the root of the repository.
fn resolved(relative: &str) -> String {
    let mut parts: Vec<String> = vec!["docs".into(), "lignindb".into(), "operator".into()];
    for component in Path::new(relative).components() {
        match component {
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(name) => parts.push(name.to_string_lossy().into_owned()),
            _ => {}
        }
    }
    parts.join("/")
}

/// The links of a line of prose given their target in the book: a chapter
/// stays a chapter, and another file of the repository is linked where the
/// book is published from.
fn book_links(line: &str) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(at) = rest.find("](") {
        let (before, after) = rest.split_at(at + 2);
        out.push_str(before);
        let Some(end) = after.find(')') else {
            rest = after;
            break;
        };
        let target = &after[..end];
        let (path, fragment) = match target.split_once('#') {
            Some((path, fragment)) => (path, format!("#{fragment}")),
            None => (target, String::new()),
        };
        let kept = path.is_empty() || path.contains("://") || CHAPTERS.contains(&path);
        match kept {
            true => out.push_str(target),
            false => out.push_str(&format!("{PUBLISHED}{}{fragment}", resolved(path))),
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// The chapter of the book that the document `name` with `text` is.
fn chapter(name: &str, text: &str) -> String {
    let mut out = format!(
        "<!-- This chapter is docs/lignindb/operator/{name}, written here by \
         lignin-tools/tests/operator_docs.rs: edit that file, then run \
         `LIGNIN_OPERATOR_DOCS=write cargo test -p lignin-tools --test operator_docs`. -->\n\n"
    );
    let mut fenced = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            fenced = !fenced;
        }
        if !fenced && line.trim_start().starts_with("<!-- runbook.sh:") {
            continue;
        }
        match fenced {
            true => out.push_str(line),
            false => out.push_str(&book_links(line)),
        }
        out.push('\n');
    }
    out
}

#[test]
fn the_directory_holds_the_chapters_and_the_script() {
    let mut names: Vec<String> = fs::read_dir(operator_dir())
        .expect("the operator documents")
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    let mut expected: Vec<String> = CHAPTERS.iter().map(|name| name.to_string()).collect();
    expected.push("runbook.sh".into());
    expected.sort();
    assert_eq!(names, expected, "a document of the directory is no chapter");
}

#[test]
fn every_command_names_what_the_workspace_has() {
    let packages = manifests();
    let mut faults = Vec::new();
    let mut checked = 0;
    for (name, text) in documents() {
        let parsed = parse(&text);
        // The runbook's table of options is of `lignin migrate`: each is to
        // be taken by that subcommand, not by any.
        if name == "migration.md" {
            for table in tables(&parsed.prose) {
                if table.header.first().map(String::as_str) != Some("Option") {
                    continue;
                }
                for (line, row) in &table.rows {
                    let option = code_spans(&row[0]).into_iter().next().unwrap_or_default();
                    let mut words = option.split_whitespace();
                    let flag = words.next().unwrap_or_default();
                    if flag.starts_with("--") {
                        checked += 1;
                        if !takes(Some("migrate"), flag) {
                            faults.push(format!("{name}:{line}: `lignin migrate` takes no {flag}"));
                        }
                        // A value given as `a|b|c` is each a value the
                        // option takes.
                        if let Some(values) = words.next().filter(|values| values.contains('|')) {
                            for value in values.split('|') {
                                let target = ["migrate", "S", "T", flag, value];
                                let survey = ["migrate", "S", "--survey", flag, value];
                                if args::parse(&os(&target)).is_err()
                                    || args::parse(&os(&survey)).is_err()
                                {
                                    faults.push(format!(
                                        "{name}:{line}: `lignin migrate` takes no {flag} {value}"
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }
        for fence in &parsed.fences {
            assert!(
                matches!(fence.info.as_str(), "sh" | "text"),
                "{name}:{}: a block is marked `sh` or `text`, not {:?}",
                fence.line,
                fence.info
            );
            if fence.info != "sh" {
                continue;
            }
            for command in commands(fence) {
                checked += 1;
                if let Some(fault) = command_fault(command.trim(), &packages) {
                    faults.push(format!(
                        "{name}:{}: `{}` {fault}",
                        fence.line,
                        command.trim()
                    ));
                }
            }
        }
        for (line, prose) in &parsed.prose {
            if spans_and_unclosed(prose).1 {
                faults.push(format!(
                    "{name}:{line}: a code span that the line does not close"
                ));
            }
            for span in code_spans(prose) {
                let tokens: Vec<&str> = span.split_whitespace().collect();
                let fault = match tokens.as_slice() {
                    ["lignin-bench", _, ..] => command_fault(&span, &packages),
                    // The tool takes `lignin SUBCOMMAND DIR [OPTIONS]`: no
                    // option before its subcommand.
                    ["lignin", first, more @ ..] if first.starts_with("--") && !more.is_empty() => {
                        Some(format!(
                            "{first} comes before the subcommand, where lignin takes none"
                        ))
                    }
                    ["lignin", rest @ ..] if !rest.is_empty() => {
                        let subcommand = rest.first().filter(|word| !word.starts_with("--"));
                        let options = rest.iter().filter(|word| word.starts_with("--"));
                        match subcommand {
                            Some(name) if !has_subcommand(name) => {
                                Some(format!("lignin has no subcommand {name}"))
                            }
                            _ => options
                                .filter(|option| !takes(subcommand.copied(), option))
                                .map(|option| format!("{option} is taken by no such subcommand"))
                                .next()
                                // A command written out with its operands is
                                // one the parser takes as it stands; a name
                                // of an option alone (`lignin migrate
                                // --batch`) is not.
                                .or_else(|| {
                                    let whole = rest.iter().skip(1).any(|word| !word.starts_with("--"))
                                        && !rest.iter().any(|word| word.contains(['…', '|']));
                                    whole
                                        .then(|| args::parse(&os(rest)).err())
                                        .flatten()
                                        .map(|error| format!("does not parse as written: {error}"))
                                }),
                        }
                    }
                    [option, ..] if option.starts_with("--") && option.len() > 2 => {
                        (!takes(None, option)).then(|| format!("no subcommand takes {option}"))
                    }
                    _ => None,
                };
                checked += 1;
                if let Some(fault) = fault {
                    faults.push(format!("{name}:{line}: `{span}`: {fault}"));
                }
            }
        }
    }
    println!("{checked} commands and code spans read");
    assert!(checked > 100, "only {checked} commands and spans were read");
    assert!(faults.is_empty(), "{}", faults.join("\n"));
}

#[test]
fn every_block_of_commands_is_bash() {
    let mut faults = Vec::new();
    let mut blocks = 0;
    for (name, text) in documents() {
        // A fence is written as runbook.sh and the book read it alike: with
        // no space before its word or after it.
        for (at, line) in text.lines().enumerate() {
            let fence = line.trim_start().strip_prefix("```");
            let indent = &line[..line.len() - line.trim_start().len()];
            if fence.is_some_and(|info| {
                line != line.trim_end()
                    || info.starts_with(char::is_whitespace)
                    || indent.contains('\t')
            }) {
                faults.push(format!(
                    "{name}:{}: a fence with a space or a tab in it",
                    at + 1
                ));
            }
        }
        for fence in parse(&text)
            .fences
            .iter()
            .filter(|fence| fence.info == "sh")
        {
            blocks += 1;
            let mut bash = std::process::Command::new("bash")
                .arg("-n")
                .stdin(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("bash");
            {
                use std::io::Write as _;
                let stdin = bash.stdin.as_mut().expect("the stdin of bash");
                stdin
                    .write_all(fence.body.join("\n").as_bytes())
                    .expect("the block");
            }
            let output = bash.wait_with_output().expect("bash -n");
            if !output.status.success() {
                faults.push(format!(
                    "{name}:{}: {}",
                    fence.line,
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
            }
        }
    }
    assert!(blocks > 10, "only {blocks} blocks were read");
    assert!(faults.is_empty(), "{}", faults.join("\n"));
}

#[test]
fn every_quoted_message_is_in_the_code() {
    let root = repository();
    let speakers: BTreeSet<String> = SPEAKERS
        .iter()
        .flat_map(|path| patterns_below(&root.join(path)))
        .collect();
    let mut by_file: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut faults = Vec::new();
    let mut quoted = 0;
    // The commands of the documents, which the rehearsal runs on a real
    // migration's output.
    let commands: String = documents()
        .iter()
        .flat_map(|(_, text)| parse(text).fences)
        .filter(|fence| fence.info == "sh")
        .flat_map(|fence| fence.body)
        // A comment is no command: what it says is looked for by nothing.
        .map(|line| match line.trim_start().starts_with('#') {
            true => String::new(),
            // A `#` after a space or a tab begins a comment.
            false => line
                .split([' ', '\t'])
                .take_while(|word| !word.starts_with('#'))
                .collect::<Vec<_>>()
                .join(" "),
        })
        .collect::<Vec<_>>()
        .join("\n");
    for (name, text) in documents() {
        let parsed = parse(&text);
        let mut in_tables = BTreeSet::new();
        for table in tables(&parsed.prose) {
            let column = |title: &str| table.header.iter().position(|cell| cell == title);
            let (Some(message), Some(code)) = (column("Message"), column("Code")) else {
                continue;
            };
            // A row names the file of its messages, or each file where a
            // message is the words of more than one type.
            for (line, row) in &table.rows {
                let spans = code_spans(&row[message]);
                let files = code_spans(&row[code]);
                if spans.is_empty() || files.is_empty() {
                    faults.push(format!(
                        "{name}:{line}: a row without a message or its file"
                    ));
                    continue;
                }
                for file in &files {
                    if is_test_code(&root.join(file)) {
                        faults.push(format!("{name}:{line}: {file} is code of tests"));
                    }
                    by_file
                        .entry(file.clone())
                        .or_insert_with(|| literal_patterns(&read(&root.join(file))));
                }
                for span in spans {
                    quoted += 1;
                    in_tables.insert((*line, span.clone()));
                    let is_of = |file: &String| {
                        by_file[file]
                            .iter()
                            .any(|pattern| pattern.trim_end_matches('\n') == span)
                    };
                    if !files.iter().any(is_of) {
                        faults.push(format!(
                            "{name}:{line}: `{span}` is no message of {}",
                            files.join(", ")
                        ));
                    }
                }
            }
        }
        for (line, prose) in &parsed.prose {
            for span in code_spans(prose) {
                // A word of a report line, `name=…` or `name=value`, is held
                // to the words the code writes before its value, whole.
                if let Some((field, value)) = span.split_once('=').filter(|(field, value)| {
                    !field.is_empty()
                        && !value.contains(char::is_whitespace)
                        && field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                }) {
                    // The code writes a number, a flag or a name there:
                    // a value is given as `…`, a number, `true` or
                    // `false`.
                    let plausible = value == "…"
                        || value == "true"
                        || value == "false"
                        || (!value.is_empty() && value.chars().all(|c| c.is_ascii_digit()));
                    if !plausible {
                        faults.push(format!(
                            "{name}:{line}: `{span}` has a value the code does not write"
                        ));
                    }
                    // A value given as itself is one a command looks for in
                    // a report, so the rehearsal holds it to real output.
                    if value != "…" && !commands.contains(span.as_str()) {
                        faults.push(format!(
                            "{name}:{line}: `{span}` is looked for by no command of the documents"
                        ));
                    }
                    quoted += 1;
                    let named = |pattern: &String| {
                        pattern.match_indices(&format!("{field}=")).any(|(at, _)| {
                            pattern[..at]
                                .chars()
                                .next_back()
                                .is_none_or(|c| !c.is_ascii_alphanumeric() && c != '_')
                        })
                    };
                    if !speakers.iter().any(named) {
                        faults.push(format!(
                            "{name}:{line}: `{span}` is in no crate of the engine"
                        ));
                    }
                    continue;
                }
                if !is_message(&span) || in_tables.contains(&(*line, span.clone())) {
                    continue;
                }
                quoted += 1;
                if !speakers.iter().any(|pattern| pattern.contains(&span)) {
                    faults.push(format!(
                        "{name}:{line}: `{span}` is in no crate of the engine"
                    ));
                }
            }
        }
    }
    println!("{quoted} quoted messages found in the code");
    assert!(quoted > 100, "only {quoted} messages were read");
    assert!(faults.is_empty(), "{}", faults.join("\n"));
}

fn bytes(n: u64) -> String {
    const KIB: u64 = 1 << 10;
    const MIB: u64 = 1 << 20;
    match n {
        0 => "0 bytes".to_owned(),
        n if n % MIB == 0 => format!("{} MiB", n / MIB),
        n if n % KIB == 0 => format!("{} KiB", n / KIB),
        n => format!("{n} bytes"),
    }
}

fn duration(time: Duration) -> String {
    match time.subsec_nanos() == 0 && !time.is_zero() {
        true => format!("{} s", time.as_secs()),
        false => format!("{} ms", time.as_millis()),
    }
}

fn percent(share: u8) -> String {
    format!("{share} %")
}

fn policy(sync: SyncPolicy) -> String {
    match sync {
        SyncPolicy::Never => "never".to_owned(),
        SyncPolicy::Interval(every) => format!("every {}", duration(every)),
        SyncPolicy::EveryCommit => "every commit".to_owned(),
    }
}

/// Every default that a document states, by the name its table gives the
/// option, as the code has it.
fn defaults() -> BTreeMap<String, String> {
    let Options {
        create_if_missing,
        page_size,
        cache_size,
        sync,
        log_file_len,
        flush,
        cleaner,
        idle,
        reserve,
        vfs: _,
    } = Options::default();
    // The file layer is a trait object: its default is held to the source
    // of `Options::default`.
    let options = read(&repository().join("lignindb/src/options.rs"));
    let vfs = match function_body(&options, "impl Default for Options {")
        .contains("vfs: Arc::new(StdVfs),")
    {
        true => "`StdVfs`".to_owned(),
        false => "another file layer than `StdVfs`".to_owned(),
    };
    // Unset, a new database takes the default size and one that is there
    // its own; set, an open of a database of another size is refused.
    let stated_page_size = match page_size {
        None => format!(
            "unset: {} for a new database",
            bytes(PageSize::DEFAULT.get() as u64)
        ),
        Some(size) => bytes(size.get() as u64),
    };
    let page_size = page_size.unwrap_or(PageSize::DEFAULT);
    // Unset, write-back's and the cleaner's settings follow the page size
    // of the database that is opened; set, they are what was given.
    let stated_flush = match flush {
        None => "unset: write-back's defaults for the database's page size",
        Some(_) => "set",
    };
    let stated_cleaner = match cleaner {
        None => "unset: the cleaner's defaults",
        Some(_) => "set",
    };
    let log_file_len = log_file_len.unwrap_or(LogOptions::default().target_file_len);
    let FlushConfig {
        max_chain_deltas,
        max_chain_bytes,
        segment_len,
        max_deferred,
        max_unwritten,
        max_pending_bytes,
        max_log_held,
        checkpoint_log_bytes,
        idle: flush_idle,
        max_failed_wait,
    } = flush.unwrap_or(FlushConfig::new(page_size));
    let CleanerConfig {
        target_dead_percent,
        checkpoint_dead_percent,
        seal_dead_percent,
        max_segment_percent,
        min_segment_len,
        max_pass_bytes,
        segment_len: cleaner_segment_len,
        value_dead_percent,
        idle: cleaner_idle,
    } = cleaner.unwrap_or(CleanerConfig::new());
    let migrate = read(&repository().join("lignin-tools/src/migrate/mod.rs"));
    assert!(
        [
            "Some(machine / 2)",
            "starts_with(\"MemTotal:\")",
            "read_to_string(\"/proc/meminfo\")",
            "kib.saturating_mul(1024)",
        ]
        .iter()
        .all(|line| migrate.contains(line)),
        "the limit of a migration is no longer half of the machine's memory"
    );
    let memory_limit =
        "half the machine's memory as `/proc/meminfo` states it; none where that file does not \
         state it"
            .to_owned();
    let mib = |bytes: usize| (bytes >> 20).to_string();
    [
        ("Options::cache_size", bytes(cache_size as u64)),
        ("Options::sync", policy(sync)),
        ("Options::page_size", stated_page_size),
        ("Options::log_file_len", bytes(log_file_len)),
        ("Options::reserve", bytes(reserve)),
        ("Options::vfs", vfs),
        ("Options::flush", stated_flush.to_owned()),
        ("Options::cleaner", stated_cleaner.to_owned()),
        ("Options::idle", duration(idle)),
        (
            "Options::create_if_missing",
            ["no", "yes"][usize::from(create_if_missing)].to_owned(),
        ),
        ("FlushConfig::max_log_held", bytes(max_log_held)),
        (
            "FlushConfig::checkpoint_log_bytes",
            bytes(checkpoint_log_bytes),
        ),
        ("FlushConfig::max_unwritten", max_unwritten.to_string()),
        (
            "FlushConfig::max_pending_bytes",
            bytes(max_pending_bytes as u64),
        ),
        (
            "FlushConfig::max_chain_deltas",
            max_chain_deltas.to_string(),
        ),
        (
            "FlushConfig::max_chain_bytes",
            bytes(max_chain_bytes as u64),
        ),
        ("FlushConfig::segment_len", bytes(segment_len)),
        ("FlushConfig::max_deferred", max_deferred.to_string()),
        ("FlushConfig::idle", duration(flush_idle)),
        ("FlushConfig::max_failed_wait", duration(max_failed_wait)),
        (
            "CleanerConfig::target_dead_percent",
            percent(target_dead_percent),
        ),
        (
            "CleanerConfig::value_dead_percent",
            percent(value_dead_percent),
        ),
        (
            "CleanerConfig::checkpoint_dead_percent",
            percent(checkpoint_dead_percent),
        ),
        (
            "CleanerConfig::seal_dead_percent",
            percent(seal_dead_percent),
        ),
        (
            "CleanerConfig::max_segment_percent",
            percent(max_segment_percent),
        ),
        ("CleanerConfig::min_segment_len", bytes(min_segment_len)),
        ("CleanerConfig::max_pass_bytes", bytes(max_pass_bytes)),
        ("CleanerConfig::segment_len", bytes(cleaner_segment_len)),
        ("CleanerConfig::idle", duration(cleaner_idle)),
        ("--cache", bytes(DEFAULT_CACHE as u64)),
        (
            "lignin migrate --batch",
            bytes(DEFAULT_MIGRATE_BATCH as u64),
        ),
        (
            "lignin migrate --progress",
            duration(DEFAULT_MIGRATE_PROGRESS),
        ),
        ("lignin migrate --memory-limit", memory_limit.clone()),
        (
            "lignin compact --timeout",
            duration(DEFAULT_COMPACT_TIMEOUT),
        ),
        ("--memory-limit MIB", memory_limit),
        ("--batch MIB", mib(DEFAULT_MIGRATE_BATCH)),
        ("--cache MIB", mib(DEFAULT_CACHE)),
        (
            "--progress SECS",
            DEFAULT_MIGRATE_PROGRESS.as_secs().to_string(),
        ),
        (
            "--unreachable refuse|leave|carry",
            format!("`{}`", Unreachable::default().name()),
        ),
    ]
    .into_iter()
    .map(|(option, default)| (option.to_owned(), default))
    .collect()
}

#[test]
fn every_message_of_an_open_a_recovery_and_a_checkpoint_is_quoted() {
    let root = repository();
    let chapter = read(&operator_dir().join("messages.md"));
    // A message counts as quoted where a row of a table gives it as its
    // message, with what it means and what to do, and names the file it is
    // in; one that is only named beside another, or quoted for another
    // file, does not. Each row: its file, its messages, and every span it
    // has.
    let mut rows: Vec<(BTreeSet<String>, BTreeSet<String>, BTreeSet<String>)> = Vec::new();
    for table in tables(&parse(&chapter).prose) {
        let column = |title: &str| table.header.iter().position(|cell| cell == title);
        let (Some(message), Some(code)) = (column("Message"), column("Code")) else {
            continue;
        };
        for (_, row) in &table.rows {
            let files = code_spans(&row[code]).into_iter().collect();
            let all = row.iter().flat_map(|cell| code_spans(cell)).collect();
            rows.push((files, code_spans(&row[message]).into_iter().collect(), all));
        }
    }
    let quoted = |file: &str, pattern: &str, with: Option<&str>| {
        rows.iter().any(|(of, messages, all)| {
            of.contains(file)
                && messages.contains(pattern)
                && with.is_none_or(|with| all.contains(with))
        })
    };
    let mut missing = Vec::new();
    let mut covered = 0;
    for (file, types) in COVERED {
        let source = read(&root.join(file));
        for name in types {
            let display = function_body(&source, &format!("impl fmt::Display for {name} {{"));
            for pattern in literal_patterns(display) {
                let pattern = pattern.trim_end_matches('\n').to_owned();
                let words = pattern.chars().filter(|c| c.is_alphabetic()).count();
                if words < 3 || PARTS.contains(&pattern.as_str()) {
                    continue;
                }
                covered += 1;
                if !quoted(file, &pattern, None) {
                    missing.push(format!("{file} ({name}): `{pattern}`"));
                }
            }
        }
    }
    for (file, before, message) in REASONS {
        let source = outside_tests(&read(&root.join(file)));
        for (at, _) in source.match_indices(before) {
            let rest = &source[at + before.len()..];
            let patterns = match before.ends_with('(') {
                true => literal_patterns(&call_arguments(rest)),
                false if rest.trim_start().starts_with('"') => literal_patterns(rest.trim_start())
                    .into_iter()
                    .take(1)
                    .collect(),
                false => continue,
            };
            for pattern in patterns {
                if pattern.chars().filter(|c| c.is_alphabetic()).count() < 3 {
                    continue;
                }
                covered += 1;
                if !quoted(
                    file,
                    &pattern,
                    Some(message).filter(|message| !message.is_empty()),
                ) {
                    missing.push(match message.is_empty() {
                        true => format!("{file} (after {before:?}): `{pattern}`, in a row of that file"),
                        false => format!(
                            "{file} (after {before:?}): `{pattern}`, in a row of that file with `{message}`"
                        ),
                    });
                }
            }
        }
    }
    for (file, start) in GROVEDB_FUNCTIONS {
        let source = outside_tests(&read(&root.join(file)));
        let body = function_body(&source, start);
        for (at, _) in body.match_indices("Error::") {
            let rest = &body[at + "Error::".len()..];
            let variant: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            let Some(arguments) = rest[variant.len()..].strip_prefix('(') else {
                continue;
            };
            for pattern in literal_patterns(&call_arguments(arguments)) {
                if pattern.chars().filter(|c| c.is_alphabetic()).count() < 3 {
                    continue;
                }
                covered += 1;
                if !quoted(file, &pattern, None) {
                    missing.push(format!("{file} ({start}…): `{pattern}`"));
                }
            }
        }
    }
    println!("{covered} messages of the error types quoted");
    assert!(covered > 50, "only {covered} messages were read");
    assert!(
        missing.is_empty(),
        "not in messages.md:\n{}",
        missing.join("\n")
    );
}

#[test]
fn every_default_is_the_codes() {
    let mut stated = BTreeMap::new();
    for (name, text) in documents() {
        for table in tables(&parse(&text).prose) {
            if table.header.get(..2) != Some(&["Option".to_owned(), "Default".to_owned()][..]) {
                continue;
            }
            for (line, row) in table.rows {
                let option = code_spans(&row[0]).into_iter().next();
                let option = option.unwrap_or_else(|| panic!("{name}:{line}: no option"));
                let earlier =
                    stated.insert(option.clone(), (row[1].clone(), format!("{name}:{line}")));
                assert!(earlier.is_none(), "{name}:{line}: {option} is stated twice");
            }
        }
    }
    let expected = defaults();
    let mut faults = Vec::new();
    for (option, default) in &expected {
        match stated.get(option) {
            None => faults.push(format!(
                "{option} is stated nowhere; its default is {default}"
            )),
            Some((said, at)) if said != default => faults.push(format!(
                "{at}: {option} is {default}, and the document says {said}"
            )),
            Some(_) => {}
        }
    }
    for (option, (_, at)) in &stated {
        if !expected.contains_key(option) {
            faults.push(format!(
                "{at}: {option} is no option this test knows the default of"
            ));
        }
    }
    assert!(faults.is_empty(), "{}", faults.join("\n"));
}

#[test]
fn the_book_has_each_document_as_a_chapter() {
    let book = repository().join("docs/book/src");
    let write = std::env::var("LIGNIN_OPERATOR_DOCS").is_ok_and(|value| value == "write");
    let summary = read(&book.join("SUMMARY.md"));
    let mut faults = Vec::new();
    for (name, text) in documents() {
        let path = book.join("lignindb").join(name);
        let expected = chapter(name, &text);
        if write {
            fs::create_dir_all(path.parent().expect("a directory")).expect("the book's directory");
            fs::write(&path, &expected).expect("the chapter");
        }
        match fs::read_to_string(&path) {
            Ok(found) if found == expected => {}
            Ok(_) => faults.push(format!("{} is not {name} as it stands", path.display())),
            Err(error) => faults.push(format!("{}: {error}", path.display())),
        }
        if !summary.contains(&format!("(lignindb/{name})")) {
            faults.push(format!("SUMMARY.md does not list lignindb/{name}"));
        }
    }
    assert!(
        faults.is_empty(),
        "{}\nLIGNIN_OPERATOR_DOCS=write cargo test -p lignin-tools --test operator_docs writes them",
        faults.join("\n")
    );
}

#[test]
fn every_link_leads_to_a_file_and_a_heading_of_it() {
    let root = repository();
    let mut faults = Vec::new();
    let mut followed = 0;
    for (name, text) in documents() {
        for (line, prose) in &parse(&text).prose {
            for target in link_targets(prose) {
                if target.contains("://") {
                    continue;
                }
                followed += 1;
                let (path, fragment) = target.split_once('#').unwrap_or((target, ""));
                let file = match path {
                    "" => root.join("docs/lignindb/operator").join(name),
                    path => root.join(resolved(path)),
                };
                if !file.exists() {
                    faults.push(format!("{name}:{line}: {target} leads to no file"));
                    continue;
                }
                let markdown = file.extension().is_some_and(|extension| extension == "md");
                if !fragment.is_empty() && markdown && !anchors(&read(&file)).contains(fragment) {
                    faults.push(format!("{name}:{line}: {target} leads to no heading"));
                }
            }
        }
    }
    println!("{followed} links followed");
    assert!(followed > 50, "only {followed} links were followed");
    assert!(faults.is_empty(), "{}", faults.join("\n"));
}

#[test]
fn the_readers_of_the_documents_read_what_they_say() {
    assert!(
        anchors("# 6.2 The procedure\n## `lignin` migrate, at last\n```\n# no\n```")
            .into_iter()
            .eq(["62-the-procedure", "lignin-migrate-at-last"])
    );
    assert_eq!(
        code_spans("a `b` and ``c `d` e`` and ` f ` and `x"),
        ["b", "c `d` e", "f"]
    );
    assert_eq!(
        cells("| `a\\|b` | c |"),
        Some(vec!["`a|b`".to_owned(), "c".to_owned()])
    );
    assert_eq!(
        words(r#"lignin migrate "$SOURCE" --survey | tee "$(dirname "$X")""#),
        ["lignin", "migrate", "$SOURCE", "--survey"]
    );
    assert_eq!(
        words("lignin stats \"$DB\" 2>&1 | tee x"),
        ["lignin", "stats", "$DB"]
    );
    let source = "let a = 'x'; // \"no\"\nwrite!(f, \"{} holds no \\\n     database {{here}}: {source}\"); r#\"raw \"q\"\"#";
    assert_eq!(
        literal_patterns(source),
        ["… holds no database {here}: …", "raw \"q\""]
    );
    assert_eq!(
        book_links("see [a](migration.md#x), [b](../engine.md#y) and [c](https://e.org)"),
        format!(
            "see [a](migration.md#x), [b]({PUBLISHED}docs/lignindb/engine.md#y) and [c](https://e.org)"
        )
    );
    assert!(takes(Some("migrate"), "--survey"));
    assert!(takes(None, "--cache"));
    assert!(!takes(Some("verify"), "--batch"));
    assert!(!takes(None, "--no-such-option"));
}

/// A report of `lignin migrate` as the comparisons of steps 7 and 11 read
/// it: the first line, what the source holds, and the counts.
fn report(last_write: u32, root: &str, aux_bytes: u32) -> String {
    format!(
        "the source /src: RocksDB identity abc, last write {last_write}; no process has it open for writing, and none can open it while this runs\n\
         the source holds:\n  root hash: {root}\n  largest Merk that is handed over: 1 nodes\n  \
         aux records: 1 ({aux_bytes} bytes)\n    LEFT BEHIND: prefix 00\nmigrate: survey nodes=1 aux=1 faults=0\n"
    )
}

/// Runs the block of `migration.md` that holds `mark` under `set -euo
/// pipefail`, in a `WORK` whose reports are `reports`, with a `lignin` that
/// prints `survey` and fails unless it takes the source. Each of `dirs`
/// names a variable that is set to a directory of its own, made with the
/// entry it names where it names one (`SOURCE` is `/src` where it is not
/// among them). Returns whether the block succeeded, and the last line it
/// printed.
fn run_block(
    mark: &str,
    reports: &[(&str, String)],
    survey: &str,
    dirs: &[(&str, &str)],
) -> (bool, String) {
    let text = read(&operator_dir().join("migration.md"));
    let blocks: Vec<String> = (parse(&text).fences.into_iter())
        .filter(|fence| fence.info == "sh" && fence.body.iter().any(|line| line.contains(mark)))
        .map(|fence| fence.body.join("\n"))
        .collect();
    assert_eq!(blocks.len(), 1, "one block of migration.md holds {mark:?}");
    let work = tempfile::tempdir().expect("a directory");
    fs::create_dir(work.path().join("reports")).expect("reports");
    for (name, body) in reports {
        fs::write(work.path().join("reports").join(name), body).expect("a report");
    }
    let script = format!(
        "set -euo pipefail\n\
         lignin() {{ printf '%s\\n' \"$SURVEY\"; case \"$SURVEY\" in *'the source can be migrated') return 0 ;; esac; return 1; }}\n{}\n",
        blocks[0]
    );
    let mut bash = std::process::Command::new("bash");
    bash.arg("-c")
        .arg(script)
        .env("WORK", work.path())
        .env("SOURCE", "/src")
        .env("TARGET", "/tgt")
        .env("SURVEY", survey.trim_end())
        .env_remove("BACK");
    for (variable, entry) in dirs {
        let dir = work.path().join(variable.to_lowercase());
        fs::create_dir(&dir).expect("a directory of the block");
        if !entry.is_empty() {
            fs::write(dir.join(entry), "").expect("an entry");
        }
        bash.env(variable, &dir);
    }
    let output = bash.output().expect("bash");
    let printed = String::from_utf8_lossy(&output.stdout);
    let last = printed.lines().last().unwrap_or("").to_owned();
    (output.status.success(), last)
}

#[test]
fn the_comparisons_of_steps_7_and_11_answer_as_written() {
    let taken = "the source can be migrated\n";
    let refused = "the source /src holds what a migration does not carry (the lines NOT CARRIED above): nothing was written by this run\n";
    let surveyed = |last_write, root: &str, aux, ending: &str| {
        format!("{}{ending}", report(last_write, root, aux))
    };
    let migrated = |last_write, root: &str, aux| {
        format!(
            "{}migrate: compared subtrees=1 data=1 differing=0 root_hash={root} seconds=0.1\n\
             migrate: verify sound=true left_hashes=0\n\
             migrated /src into /tgt: loaded, compared and verified\n",
            report(last_write, root, aux)
        )
    };
    let step_7 = |survey: String, migrate: String| {
        run_block(
            "THE COUNTS OF THE SOURCE CHANGED",
            &[("survey.txt", survey), ("migrate.txt", migrate)],
            "",
            &[],
        )
    };
    let (passed, last) = step_7(surveyed(2, "aa", 17, taken), migrated(2, "aa", 17));
    assert!(passed, "step 7 on the same source: {last}");
    let (passed, last) = step_7(surveyed(2, "aa", 17, taken), migrated(2, "aa", 4));
    assert!(
        !passed && last.contains("THE COUNTS OF THE SOURCE CHANGED"),
        "{last}"
    );
    let (passed, last) = step_7(surveyed(2, "aa", 17, taken), migrated(3, "aa", 17));
    assert!(!passed && last.contains("THE SOURCE CHANGED"), "{last}");
    // The largest Merk that is handed over is what `--unreachable` makes it.
    let (passed, last) = step_7(
        surveyed(2, "aa", 17, taken),
        migrated(2, "aa", 17).replace("1 nodes", "2 nodes"),
    );
    assert!(passed, "step 7 with another largest Merk alone: {last}");

    let step_11 = |before: String, again: String| {
        run_block("survey-again.txt", &[("survey.txt", before)], &again, &[])
    };
    let same = "SOURCE is as step 5 surveyed it";
    let other = "SOURCE IS NOT AS STEP 5 SURVEYED IT";
    let untold = "THE SURVEYS DO NOT TELL WHETHER SOURCE IS AS STEP 5 SURVEYED IT";
    let base = surveyed(2, "aa", 17, taken);
    for (case, again, answer) in [
        ("the same", base.clone(), same),
        (
            "an aux record of other bytes",
            surveyed(2, "aa", 4, taken),
            other,
        ),
        ("a write since", surveyed(3, "aa", 17, taken), other),
        ("another root hash", surveyed(2, "bb", 17, taken), other),
        (
            "another largest Merk alone",
            base.replace("1 nodes", "2 nodes"),
            same,
        ),
        ("a refusal now", surveyed(2, "aa", 17, refused), untold),
        (
            "no root hash",
            "the source /src: RocksDB identity abc, last write 2; x\n".to_owned(),
            untold,
        ),
    ] {
        let (passed, last) = step_11(base.clone(), again);
        assert_eq!(
            (passed, last.as_str()),
            (answer == same, answer),
            "step 11, {case}"
        );
    }
    let lignin_entry = "an entry that only a LigninDB database makes";
    let (passed, last) = run_block(
        "survey-again.txt",
        &[("survey.txt", base.clone())],
        &base,
        &[("SOURCE", "MANIFEST")],
    );
    assert!(
        !passed && last.contains(lignin_entry),
        "step 11, a SOURCE with MANIFEST: {last}"
    );
    let back = |dirs: &[(&str, &str)]| {
        run_block(
            "BACK=${BACK:-$SOURCE}",
            &[],
            "the source can be migrated",
            dirs,
        )
    };
    let (passed, last) = back(&[("SOURCE", "")]);
    assert!(passed, "the way back to a RocksDB SOURCE: {last}");
    let (passed, last) = back(&[("SOURCE", "MANIFEST")]);
    assert!(
        !passed && last.contains(lignin_entry),
        "the way back to a SOURCE with MANIFEST: {last}"
    );
    let (passed, last) = back(&[("SOURCE", ""), ("BACK", "wal")]);
    assert!(
        !passed && last.contains(lignin_entry),
        "the way back to a BACK with wal: {last}"
    );
    let (passed, last) = back(&[("SOURCE", "MANIFEST"), ("BACK", "")]);
    assert_eq!(
        (passed, last.as_str()),
        (true, "the source can be migrated"),
        "the way back to a BACK of RocksDB beside a checkpoint SOURCE surveys BACK"
    );
    let (passed, last) = run_block(
        "BACK=${BACK:-$SOURCE}",
        &[],
        "the source … holds no RocksDB database (no file CURRENT)",
        &[("SOURCE", ""), ("BACK", "")],
    );
    assert!(
        !passed,
        "a survey of BACK that refuses ends the block: {last}"
    );
    let alike = surveyed(2, "aa", 17, refused);
    let (passed, last) = step_11(alike.clone(), alike);
    assert_eq!(
        (passed, last.as_str()),
        (false, untold),
        "step 11, two refusals alike"
    );
}

#[test]
fn step_4_refuses_a_source_that_holds_an_entry_of_lignindb() {
    let text = read(&operator_dir().join("migration.md"));
    let fences = parse(&text).fences;
    let block = |mark: &str| -> String {
        let found: Vec<&Fence> = (fences.iter())
            .filter(|fence| fence.info == "sh" && fence.body.iter().any(|line| line.contains(mark)))
            .collect();
        assert_eq!(found.len(), 1, "one block of migration.md holds {mark:?}");
        found[0].body.join("\n")
    };
    // The functions of step 1, without its lines that install Cargo.
    let step_1 = block("apart() {");
    let functions = &step_1[step_1.find("plain_path() {").expect("plain_path")..];
    let step_4 = block("set SOURCE to the RocksDB directory");
    let run = |entries: &[&str]| {
        let root = tempfile::tempdir().expect("a directory");
        let source = root.path().join("rocksdb");
        fs::create_dir(&source).expect("the source");
        for entry in entries {
            fs::write(source.join(entry), "").expect("an entry");
        }
        let work = root.path().join("work");
        fs::create_dir(&work).expect("WORK");
        let output = std::process::Command::new("bash")
            .arg("-c")
            .arg(format!("set -euo pipefail\n{functions}\n{step_4}\n"))
            .env("SOURCE", &source)
            .env("TARGET", root.path().join("target"))
            .env("BACKUP", root.path().join("backup"))
            .env("RESTORE", root.path().join("restore"))
            .env("WORK", &work)
            .output()
            .expect("bash");
        let printed = String::from_utf8_lossy(&output.stdout).into_owned();
        (output.status.success(), printed)
    };
    let (passed, printed) = run(&["CURRENT", "IDENTITY"]);
    assert!(
        passed && printed.contains("RESTORE "),
        "a RocksDB source: {printed}"
    );
    for entry in [
        "MANIFEST",
        "MANIFEST.deleting",
        "wal",
        "seg",
        "val",
        "table",
    ] {
        let (passed, printed) = run(&["CURRENT", entry]);
        assert!(
            !passed && printed.contains("an entry that only a LigninDB database makes"),
            "a source with {entry}: {printed}"
        );
    }
}
