//! The unsafe audit, kept true: `docs/lignindb/unsafe-audit.md` against the
//! sources of the LigninDB crates.
//!
//! The audit says where unsafe code is, what each piece of it rests on and
//! which test reaches it. This test reads the sources and fails when they
//! and the audit differ:
//!
//! - the keyword `unsafe` is in the files the audit lists and in no other
//!   file of a LigninDB crate, whatever target the file belongs to
//! - every library forbids unsafe code, the two that may hold some deny it,
//!   and the one `allow` is the one on the buffer pool's `frame` module
//! - every unsafe block and `unsafe impl` has a `SAFETY:` comment directly
//!   above it, and every `unsafe fn` a `# Safety` section
//! - the unsafe code of the buffer pool is, site for site, what the audit's
//!   inventory lists, and every test the inventory names exists and is not
//!   ignored
//!
//! It reads source text. It does not run under Miri, which has nothing to
//! check in it.

#![cfg(not(miri))]

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    sync::OnceLock,
};

/// The audit, from the workspace root.
const AUDIT: &str = "docs/lignindb/unsafe-audit.md";

/// The file that holds the buffer pool's unsafe code.
const FRAME: &str = "lignin-store/src/cache/frame.rs";

/// Where the buffer pool's tests are.
const CACHE: &str = "lignin-store/src/cache";

/// The libraries that deny unsafe code, so that one of their modules can
/// allow it. Every other library forbids it.
const DENY: [&str; 2] = ["lignin-io/src/lib.rs", "lignin-store/src/lib.rs"];

/// The one `allow`, and the line that follows it.
const ALLOW: (&str, &str) = ("lignin-store/src/cache/mod.rs", "mod frame;");

/// What is scanned beside the directories of the LigninDB crates: the code
/// another crate holds for LigninDB.
const ALSO: [&str; 1] = ["merk/src/store/lignin"];

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the crate is in the workspace")
        .to_path_buf()
}

fn is_ident(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Where the string literal ends whose opening quote is at `i`.
fn string_end(chars: &[char], mut i: usize) -> usize {
    i += 1;
    while i < chars.len() && chars[i] != '"' {
        i += if chars[i] == '\\' { 2 } else { 1 };
    }
    (i + 1).min(chars.len())
}

/// Where the raw string literal ends whose hashes, or whose opening quote
/// if it has none, begin at `i`. `None` if no raw string begins there.
fn raw_string_end(chars: &[char], i: usize) -> Option<usize> {
    let hashes = chars[i..].iter().take_while(|&&c| c == '#').count();
    let closes = |at: usize| {
        chars[at] == '"'
            && chars[at + 1..]
                .iter()
                .take(hashes)
                .filter(|&&c| c == '#')
                .count()
                == hashes
    };
    (chars.get(i + hashes) == Some(&'"')).then(|| {
        (i + hashes + 1..chars.len())
            .find(|&at| closes(at))
            .map_or(chars.len(), |at| at + 1 + hashes)
    })
}

/// Where the comment ends that begins at `i`, if one does. A block comment
/// can hold another.
fn comment_end(chars: &[char], i: usize) -> Option<usize> {
    let pair = |at: usize| (chars.get(at).copied(), chars.get(at + 1).copied());
    match pair(i) {
        (Some('/'), Some('/')) => Some(
            chars[i..]
                .iter()
                .position(|&c| c == '\n')
                .map_or(chars.len(), |end| i + end),
        ),
        (Some('/'), Some('*')) => {
            let (mut at, mut depth) = (i, 0);
            while at < chars.len() {
                match pair(at) {
                    (Some('/'), Some('*')) => (at, depth) = (at + 2, depth + 1),
                    (Some('*'), Some('/')) => (at, depth) = (at + 2, depth - 1),
                    _ => at += 1,
                }
                if depth == 0 {
                    break;
                }
            }
            Some(at.min(chars.len()))
        }
        _ => None,
    }
}

/// Where the character literal ends whose opening quote is at `i`. `None`
/// for the quote of a lifetime.
fn char_end(chars: &[char], i: usize) -> Option<usize> {
    let at = |at: usize| chars.get(at).copied();
    if at(i + 1) == Some('\\') {
        let close = (i + 3..chars.len()).find(|&close| chars[close] == '\'');
        Some(close.map_or(chars.len(), |close| close + 1))
    } else {
        (at(i + 1) != Some('\'') && at(i + 2) == Some('\'')).then_some(i + 3)
    }
}

/// The source with every comment and every string and character literal
/// turned into spaces. Line breaks stay, so a place in the result is the
/// same place in the source.
fn blank(source: &str) -> String {
    let chars: Vec<char> = source.chars().collect();
    let mut out = String::with_capacity(source.len());
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Where what begins here ends, and whether it is blanked.
        let (end, blanked) = if let Some(end) = comment_end(&chars, i) {
            (end, true)
        } else if c == '"' {
            (string_end(&chars, i), true)
        } else if c == '\'' {
            char_end(&chars, i).map_or((i + 1, false), |end| (end, true))
        } else if is_ident(c) {
            let word_end = (i..chars.len())
                .find(|&at| !is_ident(chars[at]))
                .unwrap_or(chars.len());
            let word: String = chars[i..word_end].iter().collect();
            let raw = matches!(word.as_str(), "r" | "br" | "cr")
                .then(|| raw_string_end(&chars, word_end))
                .flatten();
            let prefixed = matches!(word.as_str(), "b" | "c") && chars.get(word_end) == Some(&'"');
            match raw {
                Some(end) => (end, true),
                None if prefixed => (string_end(&chars, word_end), true),
                None => (word_end, false),
            }
        } else {
            (i + 1, false)
        };
        out.extend(chars[i..end].iter().map(|&c| match c {
            '\n' => '\n',
            _ if blanked => ' ',
            c => c,
        }));
        i = end;
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Punct(char),
}

/// The words and the punctuation of a blanked source, each with its line.
fn tokens(blanked: &str) -> Vec<(Token, usize)> {
    let mut tokens = Vec::new();
    let mut line = 1;
    let mut chars = blanked.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\n' {
            line += 1;
        } else if is_ident(c) {
            let mut word = String::from(c);
            while let Some(&next) = chars.peek().filter(|next| is_ident(**next)) {
                word.push(next);
                chars.next();
            }
            tokens.push((Token::Word(word), line));
        } else if !c.is_whitespace() {
            tokens.push((Token::Punct(c), line));
        }
    }
    tokens
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Block,
    Impl,
    Fn,
    /// Anything else the keyword can open: an `extern` block, a trait, an
    /// attribute. No audited file has one.
    Other,
}

/// One use of the keyword `unsafe`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Site {
    /// Where it is: `Type::function` for a block or a function,
    /// `impl Trait for Type` for an implementation.
    name: String,
    kind: Kind,
    line: usize,
    /// For a function: whether it implements a trait's method, whose
    /// contract is the trait's.
    of_trait: bool,
}

#[derive(Debug, Clone)]
enum Scope {
    Impl {
        of: String,
        for_trait: Option<String>,
    },
    Fn(String),
    Other,
}

/// The name of a type or a trait as a header writes it: the last word
/// before its generic arguments.
fn last_name(header: &[Token]) -> String {
    let mut name = String::new();
    for token in header {
        match token {
            Token::Punct('<') => break,
            Token::Word(word) if word == "where" => break,
            Token::Word(word) => name.clone_from(word),
            Token::Punct(_) => {}
        }
    }
    name
}

/// What the header of an item opens: the tokens from `impl`, `fn`, `mod` or
/// `trait` up to the brace.
fn scope_of(header: &[Token]) -> Scope {
    let word = |token: &Token| match token {
        Token::Word(word) => Some(word.clone()),
        Token::Punct(_) => None,
    };
    match header.first().and_then(word).as_deref() {
        Some("fn") => Scope::Fn(header.get(1).and_then(word).unwrap_or_default()),
        Some("impl") => {
            let mut rest = &header[1..];
            if rest.first() == Some(&Token::Punct('<')) {
                let mut depth = 0;
                let mut end = 0;
                for (at, token) in rest.iter().enumerate() {
                    match token {
                        Token::Punct('<') => depth += 1,
                        Token::Punct('>') => depth -= 1,
                        _ => {}
                    }
                    if depth == 0 {
                        end = at + 1;
                        break;
                    }
                }
                rest = &rest[end..];
            }
            let mut depth = 0;
            let mut split = None;
            for (at, token) in rest.iter().enumerate() {
                match token {
                    Token::Punct('<') => depth += 1,
                    Token::Punct('>') => depth -= 1,
                    Token::Word(word) if word == "for" && depth == 0 => split = Some(at),
                    _ => {}
                }
            }
            match split {
                Some(at) => Scope::Impl {
                    of: last_name(&rest[at + 1..]),
                    for_trait: Some(last_name(&rest[..at])),
                },
                None => Scope::Impl {
                    of: last_name(rest),
                    for_trait: None,
                },
            }
        }
        _ => Scope::Other,
    }
}

/// Every use of the keyword `unsafe` in a source, in the order of the
/// source.
fn sites(source: &str) -> Vec<Site> {
    let tokens = tokens(&blank(source));
    let mut sites = Vec::new();
    let mut scopes: Vec<Scope> = Vec::new();
    let mut header: Option<Vec<Token>> = None;
    let mut nesting = 0usize;
    for (at, (token, line)) in tokens.iter().enumerate() {
        match token {
            Token::Word(word) if word == "unsafe" => {
                let kind = match tokens.get(at + 1).map(|(next, _)| next) {
                    Some(Token::Punct('{')) => Kind::Block,
                    Some(Token::Word(next)) if next == "impl" => Kind::Impl,
                    Some(Token::Word(next)) if next == "fn" => Kind::Fn,
                    _ => Kind::Other,
                };
                let of = scopes.iter().rev().find_map(|scope| match scope {
                    Scope::Impl { of, for_trait } => Some((of.clone(), for_trait.is_some())),
                    _ => None,
                });
                let function = scopes.iter().rev().find_map(|scope| match scope {
                    Scope::Fn(name) => Some(name.clone()),
                    _ => None,
                });
                let name = match kind {
                    Kind::Impl => {
                        let end = tokens[at..]
                            .iter()
                            .position(|(token, _)| *token == Token::Punct('{'))
                            .map_or(tokens.len(), |end| at + end);
                        let header: Vec<Token> = tokens[at + 1..end]
                            .iter()
                            .map(|(token, _)| token.clone())
                            .collect();
                        match scope_of(&header) {
                            Scope::Impl {
                                of,
                                for_trait: Some(for_trait),
                            } => format!("impl {for_trait} for {of}"),
                            Scope::Impl { of, .. } => format!("impl {of}"),
                            _ => String::from("impl"),
                        }
                    }
                    Kind::Fn => {
                        let function = match tokens.get(at + 2) {
                            Some((Token::Word(function), _)) => function.clone(),
                            _ => String::new(),
                        };
                        match &of {
                            Some((of, _)) => format!("{of}::{function}"),
                            None => function,
                        }
                    }
                    Kind::Block | Kind::Other => match (&of, function) {
                        (Some((of, _)), Some(function)) => format!("{of}::{function}"),
                        (None, Some(function)) => function,
                        (Some((of, _)), None) => of.clone(),
                        (None, None) => String::new(),
                    },
                };
                sites.push(Site {
                    name,
                    kind,
                    line: *line,
                    of_trait: kind == Kind::Fn && of.is_some_and(|(_, for_trait)| for_trait),
                });
            }
            Token::Word(word)
                if header.is_none() && matches!(word.as_str(), "impl" | "fn" | "mod" | "trait") =>
            {
                header = Some(vec![token.clone()]);
                nesting = 0;
            }
            Token::Punct('{') if nesting == 0 => {
                scopes.push(
                    header
                        .take()
                        .map_or(Scope::Other, |header| scope_of(&header)),
                );
            }
            Token::Punct('}') => {
                header = None;
                scopes.pop();
            }
            Token::Punct(';') if nesting == 0 => header = None,
            token => {
                if let Some(header) = &mut header {
                    match token {
                        Token::Punct('(' | '[') => nesting += 1,
                        Token::Punct(')' | ']') => nesting = nesting.saturating_sub(1),
                        _ => {}
                    }
                    header.push(token.clone());
                }
            }
        }
    }
    sites
}

/// What is wrong with the comments of a source's unsafe code: nothing, if
/// every block and `unsafe impl` has a `SAFETY:` comment directly above its
/// line, and every `unsafe fn` that is not a trait's method has a
/// `# Safety` section in its documentation.
fn undocumented(source: &str) -> Vec<String> {
    let lines: Vec<&str> = source.lines().map(str::trim).collect();
    let above = |line: usize, comment: &str| {
        lines[..line - 1]
            .iter()
            .rev()
            .skip_while(|above| above.starts_with("#["))
            .take_while(|above| above.starts_with(comment))
            .map(|above| above.to_string())
            .collect::<Vec<_>>()
    };
    sites(source)
        .into_iter()
        .filter(|site| match site.kind {
            Kind::Block | Kind::Impl => !above(site.line, "//")
                .iter()
                .any(|comment| !comment.starts_with("///") && comment.contains("SAFETY:")),
            Kind::Fn => {
                !site.of_trait
                    && !above(site.line, "///")
                        .iter()
                        .any(|doc| doc.contains("# Safety"))
            }
            Kind::Other => true,
        })
        .map(|site| format!("line {}: {:?} {}", site.line, site.kind, site.name))
        .collect()
}

/// Every `.rs` file under `directory`, by its path from the workspace root.
fn rust_files(root: &Path, directory: &Path, files: &mut BTreeMap<String, String>) {
    let mut entries: Vec<_> = fs::read_dir(directory)
        .unwrap_or_else(|error| panic!("{}: {error}", directory.display()))
        .map(|entry| entry.expect("a directory entry").path())
        .collect();
    entries.sort();
    for path in entries {
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if path.is_dir() {
            if name != "target" && !name.starts_with('.') {
                rust_files(root, &path, files);
            }
        } else if name.ends_with(".rs") {
            let relative = path
                .strip_prefix(root)
                .expect("under the root")
                .to_str()
                .expect("a path in UTF-8")
                .replace('\\', "/");
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
            files.insert(relative, source);
        }
    }
}

/// The sources of the LigninDB crates: every directory of the workspace
/// whose name begins with `lignin`, and what [`ALSO`] names.
fn sources() -> &'static BTreeMap<String, String> {
    static SOURCES: OnceLock<BTreeMap<String, String>> = OnceLock::new();
    SOURCES.get_or_init(read_sources)
}

fn read_sources() -> BTreeMap<String, String> {
    let root = root();
    let mut files = BTreeMap::new();
    let mut crates = 0;
    for entry in fs::read_dir(&root).expect("the workspace root") {
        let path = entry.expect("a directory entry").path();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("");
        if path.is_dir() && name.starts_with("lignin") {
            crates += 1;
            rust_files(&root, &path, &mut files);
        }
    }
    assert!(crates >= 7, "the LigninDB crates are beside this one");
    for also in ALSO {
        rust_files(&root, &root.join(also), &mut files);
    }
    files
}

/// The rows of one table of the audit, between `<!-- {name}:start -->` and
/// `<!-- {name}:end -->`: the cells of each row that begins with a name in
/// backticks.
fn table(audit: &str, name: &str) -> Vec<Vec<String>> {
    let start = format!("<!-- {name}:start -->");
    let end = format!("<!-- {name}:end -->");
    let from = audit
        .find(&start)
        .unwrap_or_else(|| panic!("the audit has no {start}"));
    let to = audit
        .find(&end)
        .unwrap_or_else(|| panic!("the audit has no {end}"));
    audit[from..to]
        .lines()
        .filter(|line| line.starts_with("| `"))
        .map(|line| {
            line.trim_matches('|')
                .split(" | ")
                .map(|cell| cell.trim().to_string())
                .collect()
        })
        .collect()
}

/// The names a cell holds in backticks.
fn names(cell: &str) -> Vec<String> {
    cell.split('`')
        .skip(1)
        .step_by(2)
        .map(str::to_string)
        .collect()
}

fn audit() -> String {
    fs::read_to_string(root().join(AUDIT)).expect("the audit")
}

/// A site as the inventory names it: where it is, what it is, and which
/// one of its kind it is there.
fn keys(sites: &[Site]) -> Vec<String> {
    let mut seen: BTreeMap<(String, Kind), usize> = BTreeMap::new();
    sites
        .iter()
        .map(|site| {
            let count = seen.entry((site.name.clone(), site.kind)).or_default();
            *count += 1;
            match site.kind {
                Kind::Block => format!("{} | block {count}", site.name),
                Kind::Fn => format!("{} | unsafe fn", site.name),
                Kind::Impl => format!("{} | unsafe impl", site.name),
                Kind::Other => format!("{} | other", site.name),
            }
        })
        .collect()
}

#[test]
fn unsafe_code_is_in_the_files_the_audit_lists_and_nowhere_else() {
    let audit = audit();
    let mut listed: BTreeSet<String> = table(&audit, "allocators")
        .iter()
        .map(|row| names(&row[0])[0].clone())
        .collect();
    assert_eq!(listed.len(), table(&audit, "allocators").len());
    listed.insert(FRAME.to_string());

    let found: BTreeSet<String> = sources()
        .iter()
        .filter(|(_, source)| !sites(source).is_empty())
        .map(|(file, _)| file.clone())
        .collect();
    assert_eq!(
        found, listed,
        "the files with unsafe code, against those {AUDIT} lists"
    );
}

/// The path a `#[path]` attribute names, from the workspace root: it is
/// taken from the directory of the file that has the attribute.
fn resolved(file: &str, path: &str) -> String {
    let mut parts: Vec<&str> = file.split('/').collect();
    parts.pop();
    for part in path.split('/') {
        match part {
            ".." => drop(parts.pop()),
            "." => {}
            part => parts.push(part),
        }
    }
    parts.join("/")
}

/// Whether `file` is a source of a library that forbids unsafe code: the
/// compiler then holds everything that file brings in to the lint.
fn in_a_library_that_forbids(sources: &BTreeMap<String, String>, file: &str) -> bool {
    let binary = file.contains("/src/bin/") || file.ends_with("/src/main.rs");
    !binary
        && file.rfind("/src/").is_some_and(|at| {
            sources
                .get(&format!("{}/src/lib.rs", &file[..at]))
                .is_some_and(|library| blank(library).contains("#![forbid(unsafe_code)]"))
        })
}

/// A crate can compile a file from anywhere: a module with a `#[path]`, or
/// an `include!`. What the scan says of the crates holds only if every such
/// file is one it read, or is compiled into a library that forbids unsafe
/// code, as the three files of `grovedb-storage` are that the cost model's
/// stand-in for that crate takes.
#[test]
fn everything_a_crate_compiles_is_among_the_files_that_are_read() {
    let sources = sources();
    let mut paths = 0;
    for (file, source) in sources {
        assert_eq!(
            occurrences(source, "include!("),
            0,
            "{file} includes a file as source, which the scan cannot follow"
        );
        for line in source.lines() {
            let Some(path) = line.trim().strip_prefix("#[path = \"") else {
                continue;
            };
            let path = path.split('"').next().expect("a first part");
            let target = resolved(file, path);
            assert!(
                sources.contains_key(&target) || in_a_library_that_forbids(sources, file),
                "{file} compiles {target}, which the scan does not read"
            );
            paths += 1;
        }
    }
    assert!(
        paths >= 2,
        "the two modules of `lignindb` that share a file"
    );
    assert!(in_a_library_that_forbids(
        sources,
        "lignin-tree/src/node/mod.rs"
    ));
    assert!(!in_a_library_that_forbids(
        sources,
        "lignin-store/src/cache/mod.rs"
    ));
    assert!(!in_a_library_that_forbids(
        sources,
        "lignin-tree/tests/node_alloc.rs"
    ));
    assert!(!in_a_library_that_forbids(
        sources,
        "lignin-testkit/src/bin/lignin-bench.rs"
    ));
    assert_eq!(resolved("a/src/b/c.rs", "../d/e.rs"), "a/src/d/e.rs");
    assert_eq!(resolved("a/src/b/c.rs", "./f.rs"), "a/src/b/f.rs");
}

#[test]
fn every_library_forbids_unsafe_code_and_one_module_is_allowed_it() {
    let sources = sources();
    let mut libraries = 0;
    for (file, source) in sources {
        let blanked = blank(source);
        let lint = |level: &str| blanked.contains(&format!("#![{level}(unsafe_code)]"));
        if file.ends_with("src/lib.rs") {
            libraries += 1;
            if DENY.contains(&file.as_str()) {
                assert!(lint("deny") && !lint("forbid"), "{file} denies unsafe code");
            } else {
                assert!(lint("forbid"), "{file} forbids unsafe code");
            }
        }
        let mentions = tokens(&blanked)
            .iter()
            .filter(|(token, _)| *token == Token::Word(String::from("unsafe_code")))
            .count();
        let expected = match file.as_str() {
            file if file.ends_with("src/lib.rs") => 1,
            file if file == ALLOW.0 => 1,
            _ => 0,
        };
        assert_eq!(mentions, expected, "{file} names the lint `unsafe_code`");
    }
    assert!(libraries >= 7, "{libraries} libraries");

    let module = &sources[ALLOW.0];
    let lines: Vec<&str> = module.lines().map(str::trim).collect();
    let at = lines
        .iter()
        .position(|line| *line == "#[allow(unsafe_code)]")
        .expect("the one allow");
    assert_eq!(lines[at + 1], ALLOW.1);
    assert!(sources.contains_key(FRAME));
}

#[test]
fn every_unsafe_block_says_why_it_is_sound() {
    let mut missing = Vec::new();
    for (file, source) in sources() {
        for site in undocumented(source) {
            missing.push(format!("{file}, {site}"));
        }
    }
    assert!(missing.is_empty(), "no safety comment: {missing:#?}");
}

#[test]
fn the_inventory_is_the_unsafe_code_of_the_buffer_pool() {
    let source = &sources()[FRAME];
    let found = keys(&sites(source));
    let rows = table(&audit(), "frame");
    let listed: Vec<String> = rows
        .iter()
        .map(|row| format!("{} | {}", names(&row[0])[0], row[1]))
        .collect();
    assert_eq!(
        found, listed,
        "the unsafe code of {FRAME}, in its order, against the inventory of {AUDIT}"
    );
    for row in &rows {
        assert!(!row[2].is_empty(), "{} says what it rests on", row[0]);
        assert!(
            !names(row.last().expect("a row has cells")).is_empty(),
            "{} names a test",
            row[0]
        );
    }
}

/// A source without its test modules, which begin at the first
/// `#[cfg(all(test` that opens a line.
fn library(source: &str) -> &str {
    source
        .find("\n#[cfg(all(test")
        .map_or(source, |tests| &source[..tests])
}

/// How many times the words and punctuation of `pattern` follow one another
/// in the source, where they do not declare a type or a function.
fn occurrences(source: &str, pattern: &str) -> usize {
    let found: Vec<Token> = tokens(&blank(source))
        .into_iter()
        .map(|(token, _)| token)
        .collect();
    let wanted: Vec<Token> = tokens(pattern)
        .into_iter()
        .map(|(token, _)| token)
        .collect();
    let declares = |at: usize| {
        at > 0 && matches!(&found[at - 1], Token::Word(word) if word == "struct" || word == "fn")
    };
    (0..found.len())
        .filter(|&at| found[at..].starts_with(&wanted) && !declares(at))
        .count()
}

#[test]
fn a_way_to_reach_a_frame_is_handed_out_where_the_audit_read_it() {
    let source = &sources()[FRAME];
    let rows = table(&audit(), "trusted");
    assert_eq!(rows.len(), 6);
    for row in rows {
        let pattern = &names(&row[0])[0];
        let listed: usize = row[1].parse().expect("a number of places");
        assert_eq!(
            occurrences(library(source), pattern),
            listed,
            "`{pattern}` in {FRAME}, against {AUDIT}"
        );
    }
    assert_eq!(occurrences("struct A(u8);\nfn a() -> A { A(1) }", "A("), 1);
    assert_eq!(
        occurrences("fn set(a: u8) {}\nfn b() { set(1); set(2) }", "set("),
        2
    );
    let with_tests = "fn b() { A(1) }\n#[cfg(all(test, x))]\nmod t { A(2) }";
    assert_eq!(occurrences(with_tests, "A("), 2);
    assert_eq!(occurrences(library(with_tests), "A("), 1);
}

/// Whether `name` is a test of the source that runs wherever the tests run:
/// a function with `#[test]` above it, and nothing above it that leaves it
/// out.
fn is_a_test_that_runs(source: &str, name: &str) -> bool {
    let lines: Vec<&str> = source.lines().map(str::trim).collect();
    let opening = format!("fn {name}(");
    lines.iter().enumerate().any(|(at, line)| {
        line.starts_with(&opening) && {
            let attributes: Vec<&str> = lines[..at]
                .iter()
                .rev()
                .take_while(|above| above.starts_with("#["))
                .copied()
                .collect();
            // A test may be left out of a build for a model, where every
            // test but the models is. Anything else that leaves it out, or
            // ignores it, is refused.
            attributes.contains(&"#[test]")
                && !attributes.iter().any(|attribute| {
                    attribute.contains("ignore")
                        || (attribute.contains("cfg") && *attribute != "#[cfg(not(loom))]")
                })
        }
    })
}

#[test]
fn every_test_the_inventory_names_is_a_test_that_runs() {
    let sources = sources();
    let audit = audit();
    let mut named = BTreeSet::new();
    for row in table(&audit, "frame") {
        named.extend(names(row.last().expect("a row has cells")));
    }
    assert!(named.len() > 10, "{} tests", named.len());
    for test in named {
        // `tests::basic::name` is in `tests/basic.rs`, and `frame::tests::name`
        // in the test module of `frame.rs`.
        let (module, name) = test.rsplit_once("::").expect("a module and a name");
        let whole = format!("{CACHE}/{}.rs", module.replace("::", "/"));
        let outer = module.rsplit_once("::").map_or(module, |(outer, _)| outer);
        let inline = format!("{CACHE}/{}.rs", outer.replace("::", "/"));
        let source = sources
            .get(&whole)
            .or_else(|| sources.get(&inline))
            .unwrap_or_else(|| panic!("{test}: no {whole} and no {inline}"));
        assert!(is_a_test_that_runs(source, name), "{test} is not a test");
    }

    // A test under a counting allocator is in the file that has the
    // allocator, and runs under it.
    for row in table(&audit, "allocators") {
        let file = &names(&row[0])[0];
        for name in names(row.last().expect("a row has cells")) {
            assert!(
                is_a_test_that_runs(&sources[file], &name),
                "{file}: {name} is not a test"
            );
        }
    }
}

#[test]
fn a_counting_allocator_only_hands_on_to_the_system_allocator() {
    let sources = sources();
    let rows = table(&audit(), "allocators");
    assert!(!rows.is_empty());
    for row in rows {
        let file = &names(&row[0])[0];
        assert!(
            file.contains("/tests/"),
            "{file} is a test target, not a part of its crate"
        );
        let source = &sources[file];
        let found = sites(source);
        let of = |kind: Kind| found.iter().filter(|site| site.kind == kind).count();
        assert_eq!(
            (
                of(Kind::Impl),
                of(Kind::Fn),
                of(Kind::Block),
                of(Kind::Other)
            ),
            (1, 4, 4, 0),
            "{file}"
        );
        assert!(
            found.iter().all(|site| match site.kind {
                Kind::Impl => site.name == "impl GlobalAlloc for CountingAllocator",
                _ => site.name.starts_with("CountingAllocator::"),
            }),
            "{file}: {found:#?}"
        );
        let lines: Vec<&str> = source.lines().collect();
        for site in found.iter().filter(|site| site.kind == Kind::Block) {
            assert!(
                lines[site.line - 1].trim().starts_with("unsafe { System."),
                "{file}, line {}",
                site.line
            );
        }
    }
}

/// The reader of sources that the checks above stand on, on a source that
/// has everything it must tell apart.
#[test]
fn the_reader_finds_unsafe_code_and_nothing_that_only_looks_like_it() {
    let source = r##"
//! unsafe { in_a_comment() }
/* unsafe { /* nested */ in_a_block_comment() } */
const TEXT: &str = "unsafe { in_a_string() } \" unsafe";
const RAW: &str = r#"unsafe { "in_a_raw_string"() }"#;
const BYTES: &[u8] = b"unsafe";
const QUOTE: char = '"';
const ESCAPED: char = '\'';
fn unsafe_code_is_a_word_of_its_own<'a>(not_unsafe: &'a str) -> &'a str { not_unsafe }

// SAFETY: a comment.
unsafe impl<T: Send> Send for Holder<T> {}

impl<'a, T> Holder<T> {
    /// # Safety
    ///
    /// A section.
    #[inline]
    unsafe fn get(&self) -> &T {
        // SAFETY: a comment.
        unsafe { &*self.0 }
    }

    fn twice(&self) -> (&T, &T) {
        let first = unsafe { self.get() };
        // SAFETY: a comment.
        (first, unsafe { self.get() })
    }
}

impl<T> std::ops::Deref for Holder<T> {
    type Target = T;

    fn deref(&self) -> &T {
        let read = || {
            // Not a safety comment.
            unsafe { self.get() }
        };
        read()
    }
}

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        /// SAFETY: documentation is not a comment on a block.
        unsafe { System.alloc(layout) }
    }
}

unsafe fn free() {}

fn free_block() {
    // SAFETY: a comment.
    #[allow(unused_unsafe)]
    unsafe { free() }
}

unsafe extern "C" {}
"##;
    let found = keys(&sites(source));
    assert_eq!(
        found,
        [
            "impl Send for Holder | unsafe impl",
            "Holder::get | unsafe fn",
            "Holder::get | block 1",
            "Holder::twice | block 1",
            "Holder::twice | block 2",
            "Holder::deref | block 1",
            "impl GlobalAlloc for Counting | unsafe impl",
            "Counting::alloc | unsafe fn",
            "Counting::alloc | block 1",
            "free | unsafe fn",
            "free_block | block 1",
            " | other",
        ]
    );
    let lines: Vec<usize> = sites(source).iter().map(|site| site.line).collect();
    assert_eq!(lines[0], 12, "a place in the blanked source is its place");
    assert_eq!(
        undocumented(source),
        [
            "line 25: Block Holder::twice",
            "line 37: Block Holder::deref",
            "line 43: Impl impl GlobalAlloc for Counting",
            "line 46: Block Counting::alloc",
            "line 50: Fn free",
            "line 58: Other ",
        ]
    );
    assert!(is_a_test_that_runs("#[test]\nfn one() {}", "one"));
    assert!(!is_a_test_that_runs("fn one() {}", "one"));
    assert!(!is_a_test_that_runs(
        "#[test]\n#[ignore]\nfn one() {}",
        "one"
    ));
    assert!(!is_a_test_that_runs(
        "#[test]\n#[cfg_attr(miri, ignore)]\nfn one() {}",
        "one"
    ));
    assert!(!is_a_test_that_runs("#[test]\nfn one() {}", "on"));
    assert!(is_a_test_that_runs(
        "#[cfg(not(loom))]\n#[test]\nfn one() {}",
        "one"
    ));
    assert!(!is_a_test_that_runs(
        "#[cfg(target_os = \"linux\")]\n#[test]\nfn one() {}",
        "one"
    ));
}
