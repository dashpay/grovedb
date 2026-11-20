//! The unsafe audit, kept true: `docs/lignindb/unsafe-audit.md` against the
//! sources of the LigninDB crates.
//!
//! The audit says where unsafe code is, what each piece of it rests on and
//! which test reaches it. This test reads the sources and fails when they
//! and the audit differ:
//!
//! - the keyword `unsafe` is in the files the audit lists and in no other
//!   file of a LigninDB crate, whatever target the file belongs to, and in
//!   no documentation test
//! - every library forbids unsafe code, the two that may hold some deny it,
//!   and the one `allow` is the one on the buffer pool's `frame` module
//! - every unsafe block, `unsafe impl` and `unsafe extern` block has a
//!   `SAFETY:` comment directly above it, and every `unsafe fn` a
//!   `# Safety` section
//! - the unsafe code of the buffer pool is, site for site, what the audit's
//!   inventory lists, and every test the inventory names exists and is not
//!   ignored
//! - the places of the buffer pool that hand out a way to reach a frame are
//!   as many as the audit read
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

/// The file with the one `allow`, and the module it is on.
const ALLOW: (&str, &str) = ("lignin-store/src/cache/mod.rs", "frame");

/// What is scanned beside the directories of the LigninDB crates: the code
/// another crate holds for LigninDB.
const ALSO: [&str; 1] = ["merk/src/store/lignin"];

/// What may leave a named test, or the module it is in, out of a build: the
/// build of the tests itself, and a build for a model, where every test but
/// the models is left out.
const CONDITIONS: [&str; 3] = ["cfg(test)", "cfg(not(loom))", "cfg(all(test,not(loom)))"];

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
            Some(at)
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Part {
    Code,
    Comment,
    Literal,
    /// The `r#` of a raw identifier.
    RawPrefix,
}

/// The pieces of a source, in order: where each begins and ends, and what
/// it is. A comment or a literal is one piece. Code comes a word or a
/// character at a time.
fn parts(chars: &[char]) -> Vec<(usize, usize, Part)> {
    let mut parts = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let (end, part) = if let Some(end) = comment_end(chars, i) {
            (end, Part::Comment)
        } else if c == '"' {
            (string_end(chars, i), Part::Literal)
        } else if c == '\'' {
            char_end(chars, i).map_or((i + 1, Part::Code), |end| (end, Part::Literal))
        } else if is_ident(c) {
            let word_end = (i..chars.len())
                .find(|&at| !is_ident(chars[at]))
                .unwrap_or(chars.len());
            let word: String = chars[i..word_end].iter().collect();
            let raw = matches!(word.as_str(), "r" | "br" | "cr")
                .then(|| raw_string_end(chars, word_end))
                .flatten();
            let prefixed = matches!(word.as_str(), "b" | "c") && chars.get(word_end) == Some(&'"');
            // `r#name` is the name, whatever keyword it spells.
            let raw_name = word == "r"
                && chars.get(word_end) == Some(&'#')
                && chars.get(word_end + 1).copied().is_some_and(is_ident);
            match raw {
                Some(end) => (end, Part::Literal),
                None if prefixed => (string_end(chars, word_end), Part::Literal),
                None if raw_name => (word_end + 1, Part::RawPrefix),
                None => (word_end, Part::Code),
            }
        } else {
            (i + 1, Part::Code)
        };
        parts.push((i, end, part));
        i = end;
    }
    parts
}

/// The source in two copies of its own shape: its code, with every comment
/// and every string and character literal turned into spaces, and its
/// comments, with everything else turned into spaces. Line breaks stay in
/// both, so a line of either is the same line of the source.
fn split(source: &str) -> (String, String) {
    let chars: Vec<char> = source.chars().collect();
    let mut code = String::with_capacity(source.len());
    let mut comments = String::with_capacity(source.len());
    for (start, end, part) in parts(&chars) {
        for &c in &chars[start..end] {
            let space = if c == '\n' { '\n' } else { ' ' };
            code.push(match part {
                Part::Code => c,
                // Joined to the name that follows, as one word.
                Part::RawPrefix => '_',
                Part::Comment | Part::Literal => space,
            });
            comments.push(if part == Part::Comment { c } else { space });
        }
    }
    (code, comments)
}

/// The code of a source: [`split`] without the comments.
fn blank(source: &str) -> String {
    split(source).0
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    Punct(char),
}

/// A token and the line it is on.
type Placed = (Token, usize);

/// The words and the punctuation of a blanked source, each with its line.
fn tokens(blanked: &str) -> Vec<Placed> {
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

fn is_word(tokens: &[Placed], at: usize, word: &str) -> bool {
    matches!(tokens.get(at), Some((Token::Word(found), _)) if found == word)
}

fn is_punct(tokens: &[Placed], at: usize, punct: char) -> bool {
    matches!(tokens.get(at), Some((Token::Punct(found), _)) if *found == punct)
}

/// Tokens as text with no spaces: `cfg(not(loom))`.
fn render(tokens: &[Placed]) -> String {
    tokens
        .iter()
        .map(|(token, _)| match token {
            Token::Word(word) => word.clone(),
            Token::Punct(punct) => punct.to_string(),
        })
        .collect()
}

/// The bracket that matches the one at `at`, which opens a group if
/// `forward` and closes one otherwise.
fn matching(tokens: &[Placed], at: usize, forward: bool) -> Option<usize> {
    let pairs = [('(', ')'), ('[', ']'), ('{', '}')];
    let Token::Punct(here) = tokens[at].0 else {
        return None;
    };
    let (open, close) = pairs
        .into_iter()
        .find(|&(open, close)| here == if forward { open } else { close })?;
    let mut depth = 0i64;
    let mut walk: Box<dyn Iterator<Item = usize>> = if forward {
        Box::new(at..tokens.len())
    } else {
        Box::new((0..=at).rev())
    };
    walk.find(|&other| {
        if is_punct(tokens, other, open) {
            depth += 1;
        } else if is_punct(tokens, other, close) {
            depth -= 1;
        }
        depth == 0
    })
}

/// The outer attributes of the item whose keyword is at `at`, each as
/// [`render`] gives it: `test`, `cfg(not(loom))`. Documentation between
/// them is no token, and the words an item begins with (`pub(super)`,
/// `unsafe`) are passed over.
fn attributes_before(tokens: &[Placed], mut at: usize) -> Vec<String> {
    let qualifiers = ["pub", "unsafe", "const", "async", "extern", "default"];
    let mut attributes = Vec::new();
    while at > 0 {
        let before = at - 1;
        let open = matching(tokens, before, false).filter(|&open| open > 0);
        at = match open {
            _ if qualifiers.iter().any(|word| is_word(tokens, before, word)) => before,
            Some(open) if is_punct(tokens, before, ')') && is_word(tokens, open - 1, "pub") => {
                open - 1
            }
            Some(open) if is_punct(tokens, before, ']') && is_punct(tokens, open - 1, '#') => {
                attributes.push(render(&tokens[open + 1..before]));
                open - 1
            }
            _ => break,
        };
    }
    attributes
}

/// The inner attributes of a source (`#![…]`), each with the number of
/// braces it is inside: none for an attribute of the file itself.
fn inner_attributes(tokens: &[Placed]) -> Vec<(usize, String)> {
    let mut depth = 0usize;
    let mut found = Vec::new();
    for at in 0..tokens.len() {
        if is_punct(tokens, at, '{') {
            depth += 1;
        } else if is_punct(tokens, at, '}') {
            depth = depth.saturating_sub(1);
        } else if is_punct(tokens, at, '#') && is_punct(tokens, at + 1, '!') {
            let close = is_punct(tokens, at + 2, '[')
                .then(|| matching(tokens, at + 2, true))
                .flatten();
            if let Some(close) = close {
                found.push((depth, render(&tokens[at + 3..close])));
            }
        }
    }
    found
}

/// Whether an attribute leaves its item out of every build but that of
/// the tests: `cfg(test)`, or `cfg(all(…))` with `test` itself as one of
/// its conditions. A condition that only names the tests (`cfg(not(test))`,
/// `cfg(any(test, …))`) does not. Nor does one that asks for them deeper
/// down (`cfg(all(all(test, unix), …))`), which is taken for library code:
/// that errs to the side of counting more.
fn is_for_tests(attribute: &str) -> bool {
    let all = attribute
        .strip_prefix("cfg(all(")
        .and_then(|rest| rest.strip_suffix("))"));
    // The conditions of the `all`, which are parted by the commas that are
    // inside no other condition.
    let mut depth = 0;
    let asks_for_tests = all.is_some_and(|conditions| {
        conditions
            .split(|c| {
                depth += i32::from(c == '(') - i32::from(c == ')');
                c == ',' && depth == 0
            })
            .any(|condition| condition == "test")
    });
    attribute == "cfg(test)" || asks_for_tests
}

/// A module that is written where it is declared: the tokens of its braces,
/// and its outer attributes.
struct Module {
    open: usize,
    close: usize,
    attributes: Vec<String>,
}

fn modules(tokens: &[Placed]) -> Vec<Module> {
    (0..tokens.len())
        .filter(|&at| is_word(tokens, at, "mod") && is_punct(tokens, at + 2, '{'))
        .filter_map(|at| {
            matching(tokens, at + 2, true).map(|close| Module {
                open: at + 2,
                close,
                attributes: attributes_before(tokens, at),
            })
        })
        .collect()
}

/// The tokens of a source without its test modules: the modules whose
/// attributes name the build of the tests.
fn library(tokens: &[Placed]) -> Vec<Placed> {
    let tests: Vec<Module> = modules(tokens)
        .into_iter()
        .filter(|module| {
            module
                .attributes
                .iter()
                .any(|attribute| is_for_tests(attribute))
        })
        .collect();
    let in_tests = |at: usize| {
        tests
            .iter()
            .any(|module| module.open <= at && at <= module.close)
    };
    (0..tokens.len())
        .filter(|&at| !in_tests(at))
        .map(|at| tokens[at].clone())
        .collect()
}

/// How many times the words and punctuation of `pattern` follow one another
/// in the tokens, where they do not declare a type or a function and are
/// not the return type before a function's body.
fn occurrences(tokens: &[Placed], pattern: &str) -> usize {
    let wanted = self::tokens(pattern);
    let matches = |at: usize| {
        wanted.iter().enumerate().all(|(offset, (token, _))| {
            tokens.get(at + offset).map(|found| &found.0) == Some(token)
        })
    };
    let declares = |at: usize| {
        at > 0
            && (is_word(tokens, at - 1, "struct")
                || is_word(tokens, at - 1, "fn")
                || (at > 1 && is_punct(tokens, at - 2, '-') && is_punct(tokens, at - 1, '>')))
    };
    (0..tokens.len())
        .filter(|&at| matches(at) && !declares(at))
        .count()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Kind {
    Block,
    Impl,
    Fn,
    /// An `unsafe extern` block: the declarations of functions that are
    /// defined outside the crate, which the compiler takes on trust.
    Extern,
    /// Anything else the keyword can open: a trait, an attribute. No
    /// audited file has one.
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
/// outside generic arguments and before any `where`.
fn last_name(header: &[Token]) -> String {
    let mut name = String::new();
    let mut depth = 0;
    for token in header {
        match token {
            Token::Punct('<') => depth += 1,
            Token::Punct('>') => depth -= 1,
            Token::Word(word) if word == "where" => break,
            Token::Word(word) if depth == 0 => name.clone_from(word),
            _ => {}
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
            let rest = &header[1..];
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
                    Some(Token::Word(next)) if next == "extern" => Kind::Extern,
                    _ => Kind::Other,
                };
                let of = scopes.iter().rev().find_map(|scope| match scope {
                    Scope::Impl { of, for_trait } => Some((of.clone(), for_trait.is_some())),
                    _ => None,
                });
                let function = if kind == Kind::Fn {
                    match tokens.get(at + 2) {
                        Some((Token::Word(function), _)) => Some(function.clone()),
                        _ => Some(String::new()),
                    }
                } else {
                    scopes.iter().rev().find_map(|scope| match scope {
                        Scope::Fn(name) => Some(name.clone()),
                        _ => None,
                    })
                };
                let name = match kind {
                    Kind::Impl => {
                        let end = (at..tokens.len())
                            .find(|&end| is_punct(&tokens, end, '{'))
                            .unwrap_or(tokens.len());
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
                    _ => [of.as_ref().map(|(of, _)| of.clone()), function]
                        .into_iter()
                        .flatten()
                        .collect::<Vec<_>>()
                        .join("::"),
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

/// What is wrong with the comments of a source's unsafe code. Nothing, if:
///
/// - every block, `unsafe impl` and `unsafe extern` block has a comment
///   directly above its line, with only attributes between, one of whose
///   lines begins `// SAFETY:`
/// - every `unsafe fn` that is not a trait's method has a `# Safety`
///   heading in the documentation directly above it
/// - no two of them share a line, and so a comment
///
/// A line counts as a comment only if it is one in the source: text inside
/// a string does not.
fn undocumented(source: &str) -> Vec<String> {
    let (code, comments) = split(source);
    let code: Vec<&str> = code.lines().map(str::trim).collect();
    let comments: Vec<&str> = comments.lines().map(str::trim).collect();
    let above = |line: usize| {
        (0..line - 1)
            .rev()
            .skip_while(|&at| code[at].starts_with("#["))
            .take_while(|&at| {
                code[at].is_empty() && comments.get(at).is_some_and(|c| !c.is_empty())
            })
            .map(|at| comments[at])
            .collect::<Vec<_>>()
    };
    let mut taken = BTreeSet::new();
    sites(source)
        .into_iter()
        .filter(|site| {
            let alone = taken.insert(site.line);
            let documented = match site.kind {
                Kind::Block | Kind::Impl | Kind::Extern => above(site.line)
                    .iter()
                    .any(|comment| comment.starts_with("// SAFETY:")),
                Kind::Fn => {
                    site.of_trait
                        || above(site.line)
                            .iter()
                            .any(|doc| doc.trim_end() == "/// # Safety")
                }
                Kind::Other => false,
            };
            !(alone && documented)
        })
        .map(|site| format!("line {}: {:?} {}", site.line, site.kind, site.name))
        .collect()
}

/// The code of a source's documentation tests: what its doc comments hold
/// between code fences that rustdoc takes for Rust, line for line, and
/// nothing else.
fn documentation_tests(source: &str) -> String {
    let is_rust = |info: &str| {
        info.split(|c: char| c == ',' || c.is_whitespace())
            .filter(|word| !word.is_empty())
            .all(|word| {
                let plain = ["rust", "ignore", "should_panic", "no_run", "compile_fail"];
                plain.contains(&word) || word.starts_with("edition") || word.starts_with("ignore-")
            })
    };
    // Outside a fence, or inside one that is Rust or is not.
    let mut fence: Option<bool> = None;
    let lines: Vec<String> = split(source)
        .1
        .lines()
        .map(|line| {
            let line = line.trim();
            let Some(doc) = line
                .strip_prefix("///")
                .or_else(|| line.strip_prefix("//!"))
            else {
                fence = None;
                return String::new();
            };
            match (doc.trim().strip_prefix("```"), fence) {
                (Some(info), None) => fence = Some(is_rust(info)),
                (Some(_), Some(_)) => fence = None,
                (None, Some(true)) => return doc.to_string(),
                (None, _) => {}
            }
            String::new()
        })
        .collect();
    lines.join("\n")
}

/// Whether a source has a block documentation comment (`/** */`, `/*! */`),
/// whose documentation tests [`documentation_tests`] does not read.
fn has_block_documentation(source: &str) -> bool {
    let chars: Vec<char> = source.chars().collect();
    parts(&chars).into_iter().any(|(start, end, part)| {
        part == Part::Comment
            && match chars[start..end] {
                ['/', '*', '!', ..] => true,
                ['/', '*', '*', after, ..] => !matches!(after, '*' | '/'),
                _ => false,
            }
    })
}

/// Every `.rs` file under `directory`, by its path from the workspace root.
/// A directory `target` that Cargo made is not source, and neither is a
/// hidden one.
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
            let built = name == "target" && path.join("CACHEDIR.TAG").exists();
            if !built && !name.starts_with('.') {
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
    SOURCES.get_or_init(|| {
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
        assert!(files.len() > 300, "{} files were read", files.len());
        files
    })
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
    let rows: Vec<Vec<String>> = audit[from..to]
        .lines()
        .filter(|line| line.starts_with("| `"))
        .map(|line| {
            line.trim_matches('|')
                .split(" | ")
                .map(|cell| cell.trim().to_string())
                .collect()
        })
        .collect();
    assert!(!rows.is_empty(), "the table `{name}` of the audit has rows");
    rows
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
                Kind::Extern => format!("{} | unsafe extern", site.name),
                Kind::Other => format!("{} | other", site.name),
            }
        })
        .collect()
}

#[test]
fn unsafe_code_is_in_the_files_the_audit_lists_and_nowhere_else() {
    let audit = audit();
    let rows = table(&audit, "allocators");
    let mut listed: BTreeSet<String> = rows.iter().map(|row| names(&row[0])[0].clone()).collect();
    assert_eq!(listed.len(), rows.len());
    listed.insert(FRAME.to_string());

    let sources = sources();
    let found: BTreeSet<String> = sources
        .iter()
        .filter(|(_, source)| !sites(source).is_empty())
        .map(|(file, _)| file.clone())
        .collect();
    assert_eq!(
        found, listed,
        "the files with unsafe code, against those {AUDIT} lists"
    );

    // A documentation test is a target of its own, which no lint of its
    // crate reaches.
    for (file, source) in sources {
        assert!(
            !has_block_documentation(source),
            "{file} has a block documentation comment, which the scan does not read"
        );
        let found = sites(&documentation_tests(source));
        assert!(
            found.is_empty(),
            "{file} has unsafe code in a documentation test: {found:?}"
        );
    }
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

/// Whether the source is a crate that has `#![{level}(unsafe_code)]` as an
/// attribute of its own, and not of a module inside it.
fn has_crate_lint(source: &str, level: &str) -> bool {
    inner_attributes(&tokens(&blank(source))).contains(&(0, format!("{level}(unsafe_code)")))
}

/// Whether `file` is a source of a library that forbids unsafe code: the
/// compiler then holds everything that file brings in to the lint. A crate
/// with a binary beside its library is not taken for one, since a file
/// under its `src` can be the binary's.
fn in_a_library_that_forbids(sources: &BTreeMap<String, String>, file: &str) -> bool {
    file.rfind("/src/").is_some_and(|at| {
        let own = |name: &str| format!("{}/src/{name}", &file[..at]);
        !file[at..].starts_with("/src/bin/")
            && !sources.contains_key(&own("main.rs"))
            && sources
                .get(&own("lib.rs"))
                .is_some_and(|library| has_crate_lint(library, "forbid"))
    })
}

/// Whether the token at `at` is inside an attribute.
fn in_attribute(tokens: &[Placed], at: usize) -> bool {
    let mut depth = 0;
    for before in (0..at).rev() {
        match tokens[before].0 {
            Token::Punct(']') => depth += 1,
            Token::Punct('[') if depth > 0 => depth -= 1,
            Token::Punct('[') => {
                return before > 0
                    && (is_punct(tokens, before - 1, '#') || is_punct(tokens, before - 1, '!'));
            }
            Token::Punct(';' | '{' | '}') => return false,
            _ => {}
        }
    }
    false
}

/// The files that the `path` attributes of a source name, as they are
/// written.
fn paths(source: &str) -> Vec<String> {
    let tokens = tokens(&blank(source));
    let lines: Vec<&str> = source.lines().collect();
    (0..tokens.len())
        .filter(|&at| {
            is_word(&tokens, at, "path")
                && is_punct(&tokens, at + 1, '=')
                && in_attribute(&tokens, at)
        })
        .map(|at| {
            // The literal is no token: it is read from the source, from the
            // line of the word on, between the first two quotes after it.
            let from = tokens[at].1 - 1;
            let rest = lines[from..].join("\n");
            let after = &rest[rest.find("path").expect("the word is on its line")..];
            after
                .split('"')
                .nth(1)
                .unwrap_or_else(|| panic!("a path after its attribute: {}", lines[from]))
                .to_string()
        })
        .collect()
}

/// A crate can compile a file from anywhere: a module with a `#[path]`, or
/// an `include!`. What the scan says of the crates holds only if every such
/// file is one it read, or is compiled into a library that forbids unsafe
/// code, as the three files of `grovedb-storage` are that the cost model's
/// stand-in for that crate takes.
#[test]
fn everything_a_crate_compiles_is_among_the_files_that_are_read() {
    let sources = sources();
    let mut found = 0;
    for (file, source) in sources {
        assert_eq!(
            occurrences(&tokens(&blank(source)), "include!"),
            0,
            "{file} includes a file as source, which the scan cannot follow"
        );
        for path in paths(source) {
            let target = resolved(file, &path);
            assert!(
                sources.contains_key(&target) || in_a_library_that_forbids(sources, file),
                "{file} compiles {target}, which the scan does not read"
            );
            found += 1;
        }
    }
    assert!(
        found >= 2,
        "the two modules of `lignindb` that share a file"
    );

    for (file, forbids) in [
        ("lignin-tree/src/node/mod.rs", true),
        ("lignin-store/src/cache/mod.rs", false),
        ("lignin-tree/tests/node_alloc.rs", false),
        ("lignin-testkit/src/bin/lignin-bench.rs", false),
    ] {
        assert_eq!(in_a_library_that_forbids(sources, file), forbids, "{file}");
    }
    assert_eq!(resolved("a/src/b/c.rs", "../d/e.rs"), "a/src/d/e.rs");
    assert_eq!(resolved("a/src/b/c.rs", "./f.rs"), "a/src/b/f.rs");
    let written = "#[path = \"a.rs\"]\nmod a;\n#[cfg_attr(unix, path = \"b.rs\")]\nmod b;\n\
                   fn f() { let path = \"c.rs\"; }\n// #[path = \"d.rs\"]\n";
    assert_eq!(paths(written), ["a.rs", "b.rs"]);
    let spread = "#[path =\n    \"a.rs\"]\nmod a;\n#[path = r#\"b.rs\"#]\nmod b;\n";
    assert_eq!(paths(spread), ["a.rs", "b.rs"]);
    assert_eq!(
        occurrences(
            &tokens("include!(\"a\"); include! { b } included!()"),
            "include!"
        ),
        2
    );
}

#[test]
fn every_library_forbids_unsafe_code_and_one_module_is_allowed_it() {
    let sources = sources();
    let mut libraries = 0;
    for (file, source) in sources {
        if file.ends_with("src/lib.rs") {
            libraries += 1;
            if DENY.contains(&file.as_str()) {
                assert!(
                    has_crate_lint(source, "deny") && !has_crate_lint(source, "forbid"),
                    "{file} denies unsafe code"
                );
            } else {
                assert!(
                    has_crate_lint(source, "forbid"),
                    "{file} forbids unsafe code"
                );
            }
        }
        let expected = usize::from(file.ends_with("src/lib.rs") || file == ALLOW.0);
        assert_eq!(
            occurrences(&tokens(&blank(source)), "unsafe_code"),
            expected,
            "{file} names the lint `unsafe_code`"
        );
    }
    assert!(libraries >= 7, "{libraries} libraries");

    let module = tokens(&blank(&sources[ALLOW.0]));
    let declared = (0..module.len())
        .find(|&at| is_word(&module, at, "mod") && is_word(&module, at + 1, ALLOW.1))
        .expect("the module with the unsafe code");
    assert_eq!(attributes_before(&module, declared), ["allow(unsafe_code)"]);
    assert!(sources.contains_key(FRAME));

    let nested = "mod inner {\n    #![forbid(unsafe_code)]\n}\n";
    assert!(!has_crate_lint(nested, "forbid"));
    assert!(has_crate_lint(
        "//! A crate.\n\n#![forbid(unsafe_code)]\n",
        "forbid"
    ));
    assert!(!has_crate_lint("// #![forbid(unsafe_code)]\n", "forbid"));
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

#[test]
fn a_way_to_reach_a_frame_is_handed_out_where_the_audit_read_it() {
    let library = library(&tokens(&blank(&sources()[FRAME])));
    let rows = table(&audit(), "trusted");
    assert_eq!(rows.len(), 11);
    for row in rows {
        let pattern = &names(&row[0])[0];
        let listed: usize = row[1].parse().expect("a number of places");
        assert_eq!(
            occurrences(&library, pattern),
            listed,
            "`{pattern}` in {FRAME}, against {AUDIT}"
        );
    }

    let count = |source: &str, pattern: &str| occurrences(&library_of(source), pattern);
    assert_eq!(count("struct A(u8);\nfn a() -> A { A(1) }", "A("), 1);
    assert_eq!(
        count("fn set(a: u8) {}\nfn b() { set(1); set(2) }", "set("),
        2
    );
    assert_eq!(count("fn new() -> Self { Self { a: 1 } }", "Self {"), 1);
    let with_tests = "#[cfg(all(test, not(loom)))]\nmod early { fn t() { A(0) } }\n\
                      fn b() { A(1) }\n#[cfg(test)]\nmod late { fn t() { A(2) } }\nmod kept { fn c() { A(3) } }\n\
                      #[cfg(not(test))]\nmod shipped { fn d() { A(4) } }\n\
                      #[cfg(any(test, feature = \"x\"))]\nmod either { fn e() { A(5) } }";
    assert_eq!(count(with_tests, "A("), 4);
    assert_eq!(occurrences(&tokens(with_tests), "A("), 6);
    for (attribute, for_tests) in [
        ("cfg(test)", true),
        ("cfg(all(test,not(loom)))", true),
        ("cfg(all(not(loom),test))", true),
        ("cfg(all(not(loom),any(unix,test),feature=))", false),
        ("cfg(all(not(test),unix))", false),
        ("cfg(not(test))", false),
        ("cfg(any(test,unix))", false),
        ("cfg_attr(test,allow(dead_code))", false),
        ("test", false),
    ] {
        assert_eq!(is_for_tests(attribute), for_tests, "{attribute}");
    }
}

fn library_of(source: &str) -> Vec<Placed> {
    library(&tokens(&blank(source)))
}

/// Whether `name` is a test of the source that runs wherever the tests run:
/// a function with `#[test]`, with nothing on it, on a module it is in or
/// on its file that ignores it or leaves it out of a build. The conditions
/// of [`CONDITIONS`] are let through, and for the function itself only the
/// one that leaves it out of a build for a model.
fn is_a_test_that_runs(source: &str, name: &str) -> bool {
    let tokens = tokens(&blank(source));
    let found: Vec<usize> = (0..tokens.len())
        .filter(|&at| {
            is_word(&tokens, at, "fn")
                && is_word(&tokens, at + 1, name)
                && is_punct(&tokens, at + 2, '(')
        })
        .collect();
    // Two functions of one name in a file, in two modules, are not told
    // apart: neither is taken.
    let [at] = found[..] else {
        return false;
    };
    let passes = |attribute: &str, conditions: &[&str]| {
        let ignores = attribute
            .split(|c| !is_ident(c))
            .any(|word| word == "ignore");
        !ignores && (!attribute.starts_with("cfg") || conditions.contains(&attribute))
    };
    let own = attributes_before(&tokens, at);
    let around = modules(&tokens)
        .into_iter()
        .filter(|module| module.open < at && at < module.close)
        .flat_map(|module| module.attributes)
        .chain(
            inner_attributes(&tokens)
                .into_iter()
                .map(|(_, attribute)| attribute),
        );
    own.iter().any(|attribute| attribute == "test")
        && own
            .iter()
            .all(|attribute| passes(attribute, &CONDITIONS[1..2]))
        && around
            .into_iter()
            .all(|attribute| passes(&attribute, &CONDITIONS))
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
        let tests = names(row.last().expect("a row has cells"));
        assert!(!tests.is_empty(), "{file} names a test");
        for name in tests {
            assert!(
                is_a_test_that_runs(&sources[file], &name),
                "{file}: {name} is not a test"
            );
        }
    }
}

/// The four methods of a counting allocator, as each of them is written:
/// the method of `System` of the same name, with the same arguments.
const HANDED_ON: [(&str, &str); 4] = [
    ("alloc", "unsafe { System.alloc(layout) }"),
    ("alloc_zeroed", "unsafe { System.alloc_zeroed(layout) }"),
    (
        "realloc",
        "unsafe { System.realloc(ptr, layout, new_size) }",
    ),
    ("dealloc", "unsafe { System.dealloc(ptr, layout) }"),
];

#[test]
fn a_counting_allocator_only_hands_on_to_the_system_allocator() {
    let sources = sources();
    for row in table(&audit(), "allocators") {
        let file = &names(&row[0])[0];
        assert!(
            file.contains("/tests/"),
            "{file} is a test target, not a part of its crate"
        );
        let source = &sources[file];
        let lines: Vec<&str> = source.lines().map(str::trim).collect();
        let found: Vec<(String, Kind, &str)> = sites(source)
            .into_iter()
            .map(|site| (site.name, site.kind, lines[site.line - 1]))
            .collect();
        let mut expected = vec![(
            String::from("impl GlobalAlloc for CountingAllocator"),
            Kind::Impl,
            "unsafe impl GlobalAlloc for CountingAllocator {",
        )];
        for (method, block) in HANDED_ON {
            let name = format!("CountingAllocator::{method}");
            let header = lines
                .iter()
                .find(|line| line.starts_with(&format!("unsafe fn {method}(")))
                .unwrap_or_else(|| panic!("{file} has no {method}"));
            expected.push((name.clone(), Kind::Fn, header));
            expected.push((name, Kind::Block, block));
        }
        assert_eq!(found, expected, "{file}");
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

fn two_on_a_line() {
    // SAFETY: one comment for two blocks.
    (unsafe { free() }, unsafe { free() });
}

fn under_a_string() {
    let _text = "
    // SAFETY: text of a string, and no comment.
    ";
    unsafe { free() }
}

fn far_from_its_comment() {
    // SAFETY: of the block, with a statement between.
    let _between = 1;
    unsafe { free() }
}

/// # Safety is named in passing, with no section.
unsafe fn loosely_documented() {}

fn calls_out() {
    // SAFETY: a comment on declarations.
    unsafe extern "C" {
        fn declared(len: usize) -> i32;
    }
    // SAFETY: a comment.
    unsafe { declared(0) };
}

unsafe trait Trusted {}
"##;
    let found = sites(source);
    assert_eq!(
        keys(&found),
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
            " | unsafe extern",
            "two_on_a_line | block 1",
            "two_on_a_line | block 2",
            "under_a_string | block 1",
            "far_from_its_comment | block 1",
            "loosely_documented | unsafe fn",
            "calls_out | unsafe extern",
            "calls_out | block 1",
            " | other",
        ]
    );
    assert!(sites("fn a_raw_name(r#unsafe: u8) -> u8 { r#unsafe }").is_empty());
    assert_eq!(
        found[0].line, 12,
        "a line of the code is its line in the source"
    );
    assert_eq!(
        undocumented(source),
        [
            "line 25: Block Holder::twice",
            "line 37: Block Holder::deref",
            "line 43: Impl impl GlobalAlloc for Counting",
            "line 46: Block Counting::alloc",
            "line 50: Fn free",
            "line 58: Extern ",
            "line 62: Block two_on_a_line",
            "line 69: Block under_a_string",
            "line 75: Block far_from_its_comment",
            "line 79: Fn loosely_documented",
            "line 90: Other ",
        ]
    );
}

#[test]
fn the_reader_knows_a_test_that_runs_from_one_that_is_left_out() {
    for (source, name, runs) in [
        ("#[test]\nfn one() {}", "one", true),
        ("fn one() {}", "one", false),
        ("#[test]\nfn one() {}", "on", false),
        ("#[test]\n#[ignore]\nfn one() {}", "one", false),
        (
            "#[ignore = \"slow\"]\n/// Documented.\n#[test]\nfn one() {}",
            "one",
            false,
        ),
        (
            "#[test]\n#[cfg_attr(miri, ignore)]\nfn one() {}",
            "one",
            false,
        ),
        (
            "#[cfg_attr(\n    miri,\n    ignore = \"no hard links\"\n)]\n#[test]\nfn one() {}",
            "one",
            false,
        ),
        ("#[cfg(not(loom))]\n#[test]\nfn one() {}", "one", true),
        (
            "#[cfg(target_os = \"linux\")]\n#[test]\nfn one() {}",
            "one",
            false,
        ),
        (
            "#[test]\n#[should_panic(expected = \"no\")]\npub(crate) fn one() {}",
            "one",
            true,
        ),
        (
            "#[cfg(all(test, not(loom)))]\nmod tests {\n    #[test]\n    fn one() {}\n}",
            "one",
            true,
        ),
        (
            "#[cfg(not(miri))]\nmod tests {\n    #[test]\n    fn one() {}\n}",
            "one",
            false,
        ),
        ("#![cfg(not(miri))]\n#[test]\nfn one() {}", "one", false),
        (
            "#![cfg(not(loom))]\n#![allow(dead_code)]\n#[test]\nfn one() {}",
            "one",
            true,
        ),
        (
            "#[cfg(not(miri))]\nmod other {}\n#[test]\nfn one() {}",
            "one",
            true,
        ),
        (
            "mod a {\n    #[test]\n    #[ignore]\n    fn one() {}\n}\nmod b {\n    #[test]\n    fn one() {}\n}",
            "one",
            false,
        ),
    ] {
        assert_eq!(is_a_test_that_runs(source, name), runs, "{source:?} {name}");
    }
}

#[test]
fn the_reader_finds_the_code_of_documentation_tests() {
    let source = "\
/// Prose with the word unsafe { in it }.
///
/// ```
/// let x = unsafe { *pointer };
/// ```
///
/// ```text
/// unsafe { shown_as_text() }
/// ```
///
/// ```no_run,edition2024
/// unsafe fn second() {}
/// ```
fn documented() {}

//! ```
//! unsafe impl Send for Third {}
//! ```

// ```
// unsafe { in_a_comment_that_is_no_documentation() }
// ```
";
    let found: Vec<(Kind, usize)> = sites(&documentation_tests(source))
        .iter()
        .map(|site| (site.kind, site.line))
        .collect();
    assert_eq!(found, [(Kind::Block, 4), (Kind::Fn, 12), (Kind::Impl, 17)]);
    assert!(!has_block_documentation(source));
    assert!(!has_block_documentation(
        "/* A comment. */ /*** Stars. ***/ /**/ \"/** text */\""
    ));
    assert!(has_block_documentation(
        "/** Documentation. */\nfn documented() {}"
    ));
    assert!(has_block_documentation("/*! Documentation of the file. */"));
}
