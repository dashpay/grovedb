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
const SPEAKERS: [&str; 10] = [
    "lignindb/src",
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
const COVERED: [(&str, &[&str]); 5] = [
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

/// Words of those types that fill a place of a longer message of theirs,
/// which the chapter quotes with `…` there.
const PARTS: [&str; 1] = ["up to …"];

/// The first words of the commands a document can name in a code span.
const COMMANDS: [&str; 18] = [
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

/// Whether `lignin` has the subcommand `name`.
fn has_subcommand(name: &str) -> bool {
    FORMS.iter().any(|form| form[0] == name)
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

/// A string literal of Rust source as a pattern of the text it formats:
/// escapes made, a line that ends in `\` joined to the next with no space
/// between, and every argument of a format string as `…`.
fn literal_patterns(source: &str) -> Vec<String> {
    let chars: Vec<char> = source.chars().collect();
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

/// The literals of every Rust file at or below `path`.
fn patterns_below(path: &Path) -> Vec<String> {
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
    for entry in entries {
        if entry.is_dir() || entry.extension().is_some_and(|extension| extension == "rs") {
            found.extend(patterns_below(&entry));
        }
    }
    found
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
                                .next(),
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
fn every_quoted_message_is_in_the_code() {
    let root = repository();
    let speakers: BTreeSet<String> = SPEAKERS
        .iter()
        .flat_map(|path| patterns_below(&root.join(path)))
        .collect();
    let mut by_file: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut faults = Vec::new();
    let mut quoted = 0;
    for (name, text) in documents() {
        let parsed = parse(&text);
        let mut in_tables = BTreeSet::new();
        for table in tables(&parsed.prose) {
            let column = |title: &str| table.header.iter().position(|cell| cell == title);
            let (Some(message), Some(code)) = (column("Message"), column("Code")) else {
                continue;
            };
            for (line, row) in &table.rows {
                let spans = code_spans(&row[message]);
                let file = code_spans(&row[code]).into_iter().next();
                let Some(file) = file.filter(|_| !spans.is_empty()) else {
                    faults.push(format!(
                        "{name}:{line}: a row without a message or its file"
                    ));
                    continue;
                };
                let patterns = by_file
                    .entry(file.clone())
                    .or_insert_with(|| literal_patterns(&read(&root.join(&file))));
                for span in spans {
                    quoted += 1;
                    in_tables.insert((*line, span.clone()));
                    if !patterns
                        .iter()
                        .any(|pattern| pattern.trim_end_matches('\n') == span)
                    {
                        faults.push(format!("{name}:{line}: `{span}` is no message of {file}"));
                    }
                }
            }
        }
        for (line, prose) in &parsed.prose {
            for span in code_spans(prose) {
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
    let page_size = page_size.unwrap_or(PageSize::DEFAULT);
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
        migrate.contains("Some(machine / 2)"),
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
        ("Options::page_size", bytes(page_size.get() as u64)),
        ("Options::log_file_len", bytes(log_file_len)),
        ("Options::reserve", bytes(reserve)),
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
        ("lignin --cache", bytes(DEFAULT_CACHE as u64)),
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
    let quoted: BTreeSet<String> = parse(&chapter)
        .prose
        .iter()
        .flat_map(|(_, line)| code_spans(line))
        .collect();
    let mut missing = Vec::new();
    let mut covered = 0;
    for (file, types) in COVERED {
        let source = read(&root.join(file));
        for name in types {
            let display = function_body(&source, &format!("impl fmt::Display for {name} {{"));
            for pattern in literal_patterns(display) {
                let pattern = pattern.trim_end_matches('\n').to_owned();
                let words = pattern.chars().filter(|c| c.is_alphabetic()).count();
                if words < 3 || !pattern.contains(' ') || PARTS.contains(&pattern.as_str()) {
                    continue;
                }
                covered += 1;
                if !quoted.contains(&pattern) {
                    missing.push(format!("{file} ({name}): `{pattern}`"));
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
