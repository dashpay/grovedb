//! What the tests of the operator documents read of a document, shared
//! by `lignin-tools/tests/operator_docs.rs` and
//! `lignin-testkit/tests/runbook_rehearsal.rs`: its fenced blocks and its
//! prose, the code spans of a line, and the simple commands of a block and
//! the words the shell gives each.

/// A fenced block of a document.
pub struct Fence {
    pub info: String,
    pub line: usize,
    pub body: Vec<String>,
}

/// What a document holds: its fenced blocks, and its other lines with
/// their numbers.
pub struct Parsed {
    pub fences: Vec<Fence>,
    pub prose: Vec<(usize, String)>,
}

pub fn parse(text: &str) -> Parsed {
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
pub fn code_spans(line: &str) -> Vec<String> {
    spans_and_unclosed(line).0
}

/// The code spans of a line, and whether a run of backticks in it opens a
/// span that the line does not close: a span over two lines, which the
/// tests would not read whole.
pub fn spans_and_unclosed(line: &str) -> (Vec<String>, bool) {
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

/// The part of a line of commands before its comment: before the first
/// `#` that no quote or `\` holds and that begins a word.
pub fn code_of(line: &str) -> &str {
    let (mut single, mut double, mut escaped) = (false, false, false);
    let mut begins_word = true;
    for (at, c) in line.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if !single => escaped = true,
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            '#' if !single && !double && begins_word => return &line[..at],
            _ => {}
        }
        begins_word = !single && !double && (c.is_whitespace() || ";&|()".contains(c));
    }
    line
}

/// The words a command line gives its program, as the shell splits them,
/// up to its comment ([`code_of`]) and the first character that ends a
/// command (`|`, `;`, `&`): a redirection (`>`, `2>&1`, `&>`, `< file`) is
/// left out with the word it names, and the words after it are read on.
pub fn words(line: &str) -> Vec<String> {
    let line = code_of(line);
    let mut words = Vec::new();
    let mut word = String::new();
    let mut in_word = false;
    // Whether the word being read is the one a redirection names.
    let mut target = false;
    let mut chars = line.chars().peekable();
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
            '>' | '<' | '&' if c != '&' || chars.peek() == Some(&'>') => {
                // The number of a descriptor before it is no word either.
                if in_word && !word.chars().all(|c| c.is_ascii_digit()) {
                    words.push(std::mem::take(&mut word));
                }
                word.clear();
                in_word = false;
                while matches!(chars.peek(), Some('>' | '<' | '&' | '|')) {
                    chars.next();
                }
                target = true;
            }
            '|' | ';' | '&' => break,
            c if c.is_whitespace() => {
                if in_word {
                    let done = std::mem::take(&mut word);
                    match std::mem::take(&mut target) {
                        true => {}
                        false => words.push(done),
                    }
                    in_word = false;
                }
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    if in_word && !target {
        words.push(word);
    }
    words
}

/// The commands of a fenced block: its lines, with a line whose code ends
/// in `\` (not its comment, [`code_of`]) joined to the next.
pub fn commands(fence: &Fence) -> Vec<String> {
    let mut commands = Vec::new();
    let mut pending = String::new();
    for line in &fence.body {
        let continued = line.strip_suffix('\\').filter(|_| code_of(line) == line);
        match continued {
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

/// The simple commands of a command line: its parts between the `;`, `&&`,
/// `||` and `|` that no quote holds, without the words of the shell's grammar
/// that begin them (`if`, `then`, `else`, `do`, `{`, `!` and the like), so
/// that a command in a condition or a branch is checked as one on a line of
/// its own. Its comment ([`code_of`]) holds no command.
pub fn simple_commands(line: &str) -> Vec<String> {
    let line = code_of(line);
    let mut parts = Vec::new();
    let mut part = String::new();
    let (mut single, mut double) = (false, false);
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\'' if !double => single = !single,
            '"' if !single => double = !double,
            ';' if !single && !double => {
                parts.push(std::mem::take(&mut part));
                continue;
            }
            '&' | '|' if !single && !double && chars.peek() == Some(&c) => {
                chars.next();
                parts.push(std::mem::take(&mut part));
                continue;
            }
            // Each command of a pipeline is one of its own; `>|` is a
            // redirection and no pipe.
            '|' if !single && !double && !part.ends_with('>') => {
                parts.push(std::mem::take(&mut part));
                continue;
            }
            _ => {}
        }
        part.push(c);
    }
    parts.push(part);
    const GRAMMAR: [&str; 10] = [
        "if", "then", "else", "elif", "do", "while", "until", "!", "{", "(",
    ];
    parts
        .into_iter()
        .map(|part| {
            let mut rest = part.trim();
            while let Some(word) = rest
                .split_whitespace()
                .next()
                .filter(|word| GRAMMAR.contains(word))
            {
                rest = rest[word.len()..].trim_start();
            }
            rest.to_owned()
        })
        .filter(|part| !part.is_empty())
        .collect()
}
