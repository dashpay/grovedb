//! The trace file: one database's steps as text.
//!
//! ```text
//! lignin-golden-trace 1
//! name documents
//! seed 0x1
//! grove-version 5
//! engine 22f163c2b82d9bc5c985a77034529a32ef9a7c04
//! workload documents
//! c8a41rd0f3
//!   step 0
//!   # Write(Insert { path: [], key: [64], element: Tree(..) })
//!   cost seeks=3 loaded=0 added=114 replaced=0 removed=- hashes=3 sinsemilla=0
//!   root 5b1e…
//! c61f0r0a7e
//! stream 1208 fbb70cc5…
//! tally failed=0 unproved=0 unverified=0
//! end 1208 3f0c…
//! ```
//!
//! The header names the trace, its seed, the grove version and the commit
//! of the engine it was recorded from. A `workload` line opens a segment,
//! the steps of one operation stream, and a `stream` line closes it with the
//! number of operations and their
//! [`StreamDigest`](crate::workload::StreamDigest). Inside a segment:
//!
//! - a step line, [`Step::line`](super::step::Step::line), for every step;
//! - or, for a segment too long for that, `@steps digest` checkpoints: the
//!   first 32 bits of the trace digest after that many steps of the trace;
//! - indented blocks, a `step` line with the step's number in the trace and
//!   then [`Step::detail`](super::step::Step::detail), for the steps written
//!   out in full. Indented lines starting with `#` are remarks: the
//!   operation and the messages of errors. They are not compared.
//!
//! The `tally` line counts the operations that failed, the queries GroveDB
//! could not prove and the proofs that do not verify: all zero for a
//! generated workload. The `end` line carries the number of steps and the
//! [`TraceDigest`](super::step::TraceDigest) of all of them.

use std::fmt::{self, Write as _};

use super::step::{differing_parts, is_step_line, Part};

const MAGIC: &str = "lignin-golden-trace 1";

/// A trace, as it is stored.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Trace {
    /// The name of the trace.
    pub name: String,
    /// The seed its streams were generated from.
    pub seed: u64,
    /// The protocol version of the grove version it ran on.
    pub grove_version: u32,
    /// The commit of the engine it was recorded from. Not compared.
    pub engine: String,
    /// Its segments, in order.
    pub segments: Vec<Segment>,
    /// How many of its steps did not succeed.
    pub tally: Tally,
    /// How many steps it has.
    pub steps: u64,
    /// The digest of all of them.
    pub digest: String,
}

/// How many steps of a trace did not succeed. A generated workload has
/// none: only a hand-written case records an operation that fails or a
/// proof that does not verify.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Tally {
    /// Operations that failed.
    pub failed: u64,
    /// Queries GroveDB could not prove.
    pub unproved: u64,
    /// Proofs that do not verify to the root hash of the committed state.
    pub unverified: u64,
}

impl Tally {
    /// Whether every operation succeeded and every proof verifies.
    pub fn is_clean(&self) -> bool {
        *self == Tally::default()
    }
}

impl fmt::Display for Tally {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "failed={} unproved={} unverified={}",
            self.failed, self.unproved, self.unverified
        )
    }
}

/// The steps of one operation stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    /// The workload the stream belongs to.
    pub workload: String,
    /// What was recorded of its steps.
    pub entries: Vec<Entry>,
    /// How many operations the stream has.
    pub ops: u64,
    /// The digest of the stream.
    pub stream: String,
}

/// One thing recorded in a segment.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Entry {
    /// The short form of the next step.
    Step(String),
    /// The first 32 bits of the trace digest after this many steps.
    Checkpoint(u64, String),
    /// A step in full.
    Detail(Detail),
}

/// A step written out in full.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Detail {
    /// The number of the step in the trace, from zero.
    pub step: u64,
    /// What is compared: the lines of
    /// [`Step::detail`](super::step::Step::detail).
    pub lines: Vec<String>,
    /// What is not: the operation, and the messages of errors.
    pub remarks: Vec<String>,
}

impl Trace {
    /// The trace as the text of its file.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "{MAGIC}");
        let _ = writeln!(out, "name {}", self.name);
        let _ = writeln!(out, "seed {:#x}", self.seed);
        let _ = writeln!(out, "grove-version {}", self.grove_version);
        let _ = writeln!(out, "engine {}", self.engine);
        for segment in &self.segments {
            let _ = writeln!(out, "workload {}", segment.workload);
            for entry in &segment.entries {
                match entry {
                    Entry::Step(line) => {
                        let _ = writeln!(out, "{line}");
                    }
                    Entry::Checkpoint(steps, digest) => {
                        let _ = writeln!(out, "@{steps} {digest}");
                    }
                    Entry::Detail(detail) => {
                        let _ = writeln!(out, "  step {}", detail.step);
                        for remark in &detail.remarks {
                            let _ = writeln!(out, "  # {}", remark.replace('\n', " "));
                        }
                        for line in &detail.lines {
                            let _ = writeln!(out, "  {line}");
                        }
                    }
                }
            }
            let _ = writeln!(out, "stream {} {}", segment.ops, segment.stream);
        }
        let _ = writeln!(out, "tally {}", self.tally);
        let _ = writeln!(out, "end {} {}", self.steps, self.digest);
        out
    }

    /// Reads the text of a trace file.
    pub fn parse(text: &str) -> Result<Trace, ParseError> {
        let mut lines = text
            .lines()
            .enumerate()
            .map(|(index, line)| (index + 1, line));
        let mut header = |key: &str| -> Result<(usize, String), ParseError> {
            let (number, line) = lines
                .next()
                .ok_or_else(|| ParseError::new(0, format!("the file ends before `{key}`")))?;
            let value = if key.is_empty() {
                Some(line)
            } else {
                line.strip_prefix(key)
                    .and_then(|rest| rest.strip_prefix(' '))
            };
            match value {
                Some(value) if !value.is_empty() => Ok((number, value.to_string())),
                _ => Err(ParseError::new(
                    number,
                    format!("expected `{key} …`, found `{line}`"),
                )),
            }
        };
        let (number, magic) = header("")?;
        if magic != MAGIC {
            return Err(ParseError::new(
                number,
                format!("expected `{MAGIC}`, found `{magic}`"),
            ));
        }
        let (_, name) = header("name")?;
        let (number, seed) = header("seed")?;
        let seed = seed
            .strip_prefix("0x")
            .and_then(|digits| u64::from_str_radix(digits, 16).ok())
            .ok_or_else(|| ParseError::new(number, format!("`{seed}` is not a hex seed")))?;
        let (number, grove_version) = header("grove-version")?;
        let grove_version = number_at(number, &grove_version)?;
        let grove_version = u32::try_from(grove_version)
            .map_err(|_| ParseError::new(number, "the grove version is out of range".into()))?;
        let (_, engine) = header("engine")?;

        let mut segments: Vec<Segment> = Vec::new();
        let mut open: Option<Segment> = None;
        let mut tally = None;
        let mut end = None;
        for (number, line) in lines {
            let error = |message: String| ParseError::new(number, message);
            if end.is_some() {
                return Err(error(format!("`{line}` follows the end of the trace")));
            }
            if let Some(indented) = line.strip_prefix("  ") {
                let segment = open
                    .as_mut()
                    .ok_or_else(|| error("a step is written outside a workload".into()))?;
                if let Some(step) = indented.strip_prefix("step ") {
                    segment.entries.push(Entry::Detail(Detail {
                        step: number_at(number, step)?,
                        lines: Vec::new(),
                        remarks: Vec::new(),
                    }));
                    continue;
                }
                let Some(Entry::Detail(detail)) = segment.entries.last_mut() else {
                    return Err(error(format!("`{indented}` belongs to no step")));
                };
                match indented.strip_prefix('#') {
                    Some(remark) => detail.remarks.push(remark.trim_start().to_string()),
                    None => detail.lines.push(indented.to_string()),
                }
            } else if let Some(workload) = line.strip_prefix("workload ") {
                if open.is_some() {
                    return Err(error("a workload opens inside another".into()));
                }
                open = Some(Segment {
                    workload: workload.to_string(),
                    entries: Vec::new(),
                    ops: 0,
                    stream: String::new(),
                });
            } else if let Some(stream) = line.strip_prefix("stream ") {
                let mut segment = open
                    .take()
                    .ok_or_else(|| error("a stream closes no workload".into()))?;
                let (ops, digest) = count_and_digest(number, stream, 64)?;
                segment.ops = ops;
                segment.stream = digest;
                segments.push(segment);
            } else if let Some(checkpoint) = line.strip_prefix('@') {
                let segment = open
                    .as_mut()
                    .ok_or_else(|| error("a checkpoint is written outside a workload".into()))?;
                let (steps, digest) = count_and_digest(number, checkpoint, 8)?;
                segment.entries.push(Entry::Checkpoint(steps, digest));
            } else if let Some(counts) = line.strip_prefix("tally ") {
                if open.is_some() || tally.is_some() {
                    return Err(error("the tally is not where it belongs".into()));
                }
                tally = Some(tally_at(number, counts)?);
            } else if let Some(summary) = line.strip_prefix("end ") {
                if open.is_some() {
                    return Err(error("the trace ends inside a workload".into()));
                }
                if tally.is_none() {
                    return Err(error("the trace ends without a tally".into()));
                }
                end = Some(count_and_digest(number, summary, 64)?);
            } else if is_step_line(line) {
                let segment = open
                    .as_mut()
                    .ok_or_else(|| error("a step is written outside a workload".into()))?;
                segment.entries.push(Entry::Step(line.to_string()));
            } else {
                return Err(error(format!("`{line}` is not a line of a trace")));
            }
        }
        let (steps, digest) =
            end.ok_or_else(|| ParseError::new(0, "the file has no `end` line".into()))?;
        Ok(Trace {
            name,
            seed,
            grove_version,
            engine,
            segments,
            tally: tally.unwrap_or_default(),
            steps,
            digest,
        })
    }

    /// The step written out in full under number `step`, if there is one.
    pub fn detail(&self, step: u64) -> Option<&Detail> {
        self.details().find(|detail| detail.step == step)
    }

    /// Every step written out in full, in order.
    pub fn details(&self) -> impl Iterator<Item = &Detail> {
        self.segments
            .iter()
            .flat_map(|segment| &segment.entries)
            .filter_map(|entry| match entry {
                Entry::Detail(detail) => Some(detail),
                Entry::Step(_) | Entry::Checkpoint(..) => None,
            })
    }

    /// How this trace, replayed, differs from the `expected` one. Empty when
    /// every compared value is equal. The engine commit and the remarks are
    /// not compared.
    pub fn differences_from(&self, expected: &Trace) -> Vec<Difference> {
        let mut differences = Vec::new();
        let mut differ = |step: Option<u64>, what: String| {
            differences.push(Difference { step, what });
        };
        if self.name != expected.name {
            differ(
                None,
                format!("the trace is named {}, not {}", self.name, expected.name),
            );
        }
        if self.seed != expected.seed {
            differ(
                None,
                format!("the seed is {:#x}, not {:#x}", self.seed, expected.seed),
            );
        }
        if self.grove_version != expected.grove_version {
            differ(
                None,
                format!(
                    "the grove version is {}, not {}",
                    self.grove_version, expected.grove_version
                ),
            );
        }
        let workloads = |trace: &Trace| -> Vec<String> {
            trace
                .segments
                .iter()
                .map(|segment| segment.workload.clone())
                .collect()
        };
        if workloads(self) != workloads(expected) {
            differ(
                None,
                format!(
                    "the workloads are {:?}, not {:?}",
                    workloads(self),
                    workloads(expected)
                ),
            );
            return differences;
        }
        let mut step = 0u64;
        for (actual, expected) in self.segments.iter().zip(&expected.segments) {
            let workload = &expected.workload;
            if (actual.ops, &actual.stream) != (expected.ops, &expected.stream) {
                differ(
                    None,
                    format!(
                        "the operation stream of {workload} changed: {} operations with digest \
                         {}, recorded {} with {}",
                        actual.ops, actual.stream, expected.ops, expected.stream
                    ),
                );
                return differences;
            }
            if actual.entries.len() != expected.entries.len() {
                differ(
                    None,
                    format!(
                        "{workload} is laid out in {} entries, recorded in {}",
                        actual.entries.len(),
                        expected.entries.len()
                    ),
                );
                return differences;
            }
            for (actual, expected) in actual.entries.iter().zip(&expected.entries) {
                match (actual, expected) {
                    (Entry::Step(actual), Entry::Step(expected)) => {
                        if actual != expected {
                            let parts = differing_parts(expected, actual).unwrap_or_default();
                            let parts: Vec<&str> = parts.into_iter().map(Part::name).collect();
                            differ(
                                Some(step),
                                format!(
                                    "{workload}: {} differ: {actual}, recorded {expected}",
                                    parts.join(" and ")
                                ),
                            );
                        }
                        step += 1;
                    }
                    (
                        Entry::Checkpoint(actual_steps, actual),
                        Entry::Checkpoint(expected_steps, expected),
                    ) if actual_steps == expected_steps => {
                        if actual != expected {
                            differ(
                                Some(step),
                                format!(
                                    "{workload}: a step from {step} to {} differs: the digest \
                                     there is {actual}, recorded {expected}",
                                    expected_steps.saturating_sub(1)
                                ),
                            );
                        }
                        step = *expected_steps;
                    }
                    (Entry::Detail(actual), Entry::Detail(expected))
                        if actual.step == expected.step =>
                    {
                        if actual.lines != expected.lines {
                            differ(
                                Some(expected.step),
                                format!(
                                    "{workload}: the step in full differs\n{}",
                                    line_differences(&expected.lines, &actual.lines)
                                ),
                            );
                        }
                    }
                    _ => {
                        differ(
                            Some(step),
                            format!("{workload}: the trace is laid out differently from here"),
                        );
                        return differences;
                    }
                }
            }
        }
        if self.tally != expected.tally {
            differ(
                None,
                format!("the tally is {}, recorded {}", self.tally, expected.tally),
            );
        }
        if (self.steps, &self.digest) != (expected.steps, &expected.digest) {
            differ(
                None,
                format!(
                    "the trace has {} steps with digest {}, recorded {} with {}",
                    self.steps, self.digest, expected.steps, expected.digest
                ),
            );
        }
        differences
    }
}

/// The lines two renderings of a step do not share, recorded first.
fn line_differences(expected: &[String], actual: &[String]) -> String {
    let mut out = String::new();
    for index in 0..expected.len().max(actual.len()) {
        let (expected, actual) = (expected.get(index), actual.get(index));
        if expected == actual {
            continue;
        }
        if let Some(expected) = expected {
            let _ = writeln!(out, "      recorded: {}", shortened(expected));
        }
        if let Some(actual) = actual {
            let _ = writeln!(out, "      replayed: {}", shortened(actual));
        }
    }
    out.pop();
    out
}

/// A line cut to what a report can show: a proof runs to thousands of
/// characters.
fn shortened(line: &str) -> String {
    const LIMIT: usize = 200;
    match line.char_indices().nth(LIMIT) {
        Some((end, _)) => format!("{}… ({} characters)", &line[..end], line.chars().count()),
        None => line.to_string(),
    }
}

fn tally_at(line: usize, text: &str) -> Result<Tally, ParseError> {
    let mut fields = text.split(' ');
    let mut count = |name: &str| -> Result<u64, ParseError> {
        let field = fields.next().unwrap_or_default();
        let value = field
            .strip_prefix(name)
            .and_then(|rest| rest.strip_prefix('='))
            .ok_or_else(|| ParseError::new(line, format!("expected `{name}=…` in the tally")))?;
        number_at(line, value)
    };
    let tally = Tally {
        failed: count("failed")?,
        unproved: count("unproved")?,
        unverified: count("unverified")?,
    };
    match fields.next() {
        Some(extra) => Err(ParseError::new(
            line,
            format!("`{extra}` follows the tally"),
        )),
        None => Ok(tally),
    }
}

fn number_at(line: usize, text: &str) -> Result<u64, ParseError> {
    text.parse()
        .map_err(|_| ParseError::new(line, format!("`{text}` is not a number")))
}

fn count_and_digest(
    line: usize,
    text: &str,
    hex_digits: usize,
) -> Result<(u64, String), ParseError> {
    let (count, digest) = text
        .split_once(' ')
        .ok_or_else(|| ParseError::new(line, format!("`{text}` is not a count and a digest")))?;
    let is_hex = digest
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    if digest.len() != hex_digits || !is_hex {
        return Err(ParseError::new(
            line,
            format!("`{digest}` is not a digest of {hex_digits} hex digits"),
        ));
    }
    Ok((number_at(line, count)?, digest.to_string()))
}

/// One way a replayed trace differs from the recorded one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Difference {
    /// The step it is at, when it is at one.
    pub step: Option<u64>,
    /// What differs.
    pub what: String,
}

impl fmt::Display for Difference {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.step {
            Some(step) => write!(f, "step {step}: {}", self.what),
            None => write!(f, "{}", self.what),
        }
    }
}

/// Why a file is not a trace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// The line the problem is at, from one. Zero for a missing line.
    pub line: usize,
    /// What is wrong with it.
    pub message: String,
}

impl ParseError {
    fn new(line: usize, message: String) -> Self {
        Self { line, message }
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.line {
            0 => write!(f, "{}", self.message),
            line => write!(f, "line {line}: {}", self.message),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIGEST: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";

    fn sample() -> Trace {
        Trace {
            name: "suite".to_string(),
            seed: 0x4C49,
            grove_version: 3,
            engine: "abc123".to_string(),
            segments: vec![
                Segment {
                    workload: "documents".to_string(),
                    entries: vec![
                        Entry::Step("c0001r0002".to_string()),
                        Entry::Detail(Detail {
                            step: 0,
                            lines: vec!["cost seeks=1".to_string(), "root 00".to_string()],
                            remarks: vec!["Write(Insert)".to_string()],
                        }),
                        Entry::Step("c0003v0004q0005p0006".to_string()),
                    ],
                    ops: 2,
                    stream: DIGEST.to_string(),
                },
                Segment {
                    workload: "documents-reference".to_string(),
                    entries: vec![
                        Entry::Checkpoint(1000, "0a0b0c0d".to_string()),
                        Entry::Checkpoint(1500, "1a1b1c1d".to_string()),
                        Entry::Detail(Detail {
                            step: 1499,
                            lines: vec!["cost seeks=9".to_string()],
                            remarks: Vec::new(),
                        }),
                    ],
                    ops: 1498,
                    stream: DIGEST.to_string(),
                },
            ],
            tally: Tally {
                failed: 1,
                unproved: 2,
                unverified: 3,
            },
            steps: 1500,
            digest: DIGEST.to_string(),
        }
    }

    #[test]
    fn a_trace_reads_back_as_it_was_written() {
        let trace = sample();
        let text = trace.render();
        assert_eq!(
            text,
            format!(
                "lignin-golden-trace 1\nname suite\nseed 0x4c49\ngrove-version 3\nengine \
                 abc123\nworkload documents\nc0001r0002\n  step 0\n  # Write(Insert)\n  cost \
                 seeks=1\n  root 00\nc0003v0004q0005p0006\nstream 2 {DIGEST}\nworkload \
                 documents-reference\n@1000 0a0b0c0d\n@1500 1a1b1c1d\n  step 1499\n  cost \
                 seeks=9\nstream 1498 {DIGEST}\ntally failed=1 unproved=2 unverified=3\nend 1500 \
                 {DIGEST}\n"
            )
        );
        assert_eq!(Trace::parse(&text), Ok(trace.clone()));
        assert_eq!(trace.detail(0).map(|detail| detail.lines.len()), Some(2));
        assert_eq!(trace.detail(1499).map(|detail| detail.lines.len()), Some(1));
        assert_eq!(trace.detail(1), None);
        assert_eq!(trace.details().count(), 2);
        assert!(trace.differences_from(&trace).is_empty());
    }

    #[test]
    fn a_remark_stays_on_one_line() {
        let mut trace = sample();
        let Entry::Detail(detail) = &mut trace.segments[0].entries[1] else {
            panic!("the sample has a step in full");
        };
        detail.remarks = vec!["two\nlines".to_string(), String::new()];
        let read = Trace::parse(&trace.render()).expect("parse");
        assert_eq!(
            read.detail(0).expect("detail").remarks,
            ["two lines".to_string(), String::new()]
        );
    }

    #[test]
    fn what_is_not_a_trace_is_refused_with_its_line() {
        let good = sample().render();
        let lines: Vec<&str> = good.lines().collect();
        let with = |index: usize, replacement: &str| {
            let mut lines = lines.clone();
            lines[index] = replacement;
            lines.join("\n")
        };
        let without = |index: usize| {
            let mut lines = lines.clone();
            lines.remove(index);
            lines.join("\n")
        };
        let cases: Vec<(String, usize, &str)> = vec![
            (String::new(), 0, "the file ends before ``"),
            (
                with(0, "lignin-golden-trace 2"),
                1,
                "expected `lignin-golden-trace 1`",
            ),
            (
                "lignin-golden-trace 1".to_string(),
                0,
                "the file ends before `name`",
            ),
            (
                with(1, "title suite"),
                2,
                "expected `name …`, found `title suite`",
            ),
            (with(1, "name "), 2, "expected `name …`"),
            (with(1, "name"), 2, "expected `name …`"),
            (with(2, "seed 19"), 3, "`19` is not a hex seed"),
            (with(2, "seed 0xzz"), 3, "`0xzz` is not a hex seed"),
            (with(3, "grove-version five"), 4, "`five` is not a number"),
            (with(3, "grove-version 4294967296"), 4, "out of range"),
            (without(4), 5, "expected `engine …`"),
            (without(5), 6, "a step is written outside a workload"),
            (
                with(5, "  step 0"),
                6,
                "a step is written outside a workload",
            ),
            (
                with(5, "@5 0a0b0c0d"),
                6,
                "a checkpoint is written outside a workload",
            ),
            (with(5, "stream 2 00"), 6, "a stream closes no workload"),
            (
                with(6, "workload again"),
                7,
                "a workload opens inside another",
            ),
            (with(6, "c00"), 7, "`c00` is not a line of a trace"),
            (with(7, "  step x"), 8, "`x` is not a number"),
            (
                with(7, "  cost seeks=1"),
                8,
                "`cost seeks=1` belongs to no step",
            ),
            (with(12, "stream 2"), 13, "`2` is not a count and a digest"),
            (
                with(12, "stream two 00"),
                13,
                "is not a digest of 64 hex digits",
            ),
            (
                with(12, &format!("stream two {DIGEST}")),
                13,
                "`two` is not a number",
            ),
            (
                with(14, "@1000 0A0B0C0D"),
                15,
                "is not a digest of 8 hex digits",
            ),
            (
                with(14, "@1000 0a0b0c"),
                15,
                "is not a digest of 8 hex digits",
            ),
            (
                with(18, &format!("end 1500 {DIGEST}")),
                19,
                "the trace ends inside a workload",
            ),
            (
                with(18, "tally failed=0 unproved=0 unverified=0"),
                19,
                "the tally is not where",
            ),
            (
                with(20, "tally failed=0 unproved=0 unverified=0"),
                21,
                "the tally is not where",
            ),
            (without(19), 20, "the trace ends without a tally"),
            (
                with(19, "tally failed=0 unproved=0"),
                20,
                "expected `unverified=…` in the tally",
            ),
            (
                with(19, "tally failed=0 proved=0 unverified=0"),
                20,
                "expected `unproved=…`",
            ),
            (
                with(19, "tally failed=x unproved=0 unverified=0"),
                20,
                "`x` is not a number",
            ),
            (
                with(19, "tally failed=0 unproved=0 unverified=0 more=1"),
                20,
                "`more=1` follows the tally",
            ),
            (without(20), 0, "the file has no `end` line"),
            (format!("{good}c0001\n"), 22, "follows the end of the trace"),
        ];
        for (text, line, message) in cases {
            let error = Trace::parse(&text).expect_err(&text);
            assert_eq!(error.line, line, "{error}");
            assert!(error.message.contains(message), "{error} lacks {message:?}");
            let shown = error.to_string();
            assert_eq!(
                shown.starts_with(&format!("line {line}: ")),
                line != 0,
                "{shown}"
            );
        }
    }

    #[test]
    fn every_compared_value_is_compared() {
        let recorded = sample();
        let differences = |change: &dyn Fn(&mut Trace)| {
            let mut replayed = recorded.clone();
            change(&mut replayed);
            replayed
                .differences_from(&recorded)
                .iter()
                .map(Difference::to_string)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            differences(&|trace| trace.engine = "other".to_string()),
            Vec::<String>::new()
        );
        assert_eq!(
            differences(&|trace| {
                let Entry::Detail(detail) = &mut trace.segments[0].entries[1] else {
                    panic!("the sample has a step in full");
                };
                detail.remarks.clear();
            }),
            Vec::<String>::new()
        );
        assert_eq!(
            differences(&|trace| {
                trace.name = "other".to_string();
                trace.seed = 1;
                trace.grove_version = 4;
            }),
            [
                "the trace is named other, not suite",
                "the seed is 0x1, not 0x4c49",
                "the grove version is 4, not 3",
            ]
        );
        assert_eq!(
            differences(&|trace| trace.segments[1].workload = "tokens".to_string()),
            [
                "the workloads are [\"documents\", \"tokens\"], not [\"documents\", \
              \"documents-reference\"]"
            ]
        );
        assert_eq!(
            differences(&|trace| {
                trace.segments.pop();
            })
            .len(),
            1
        );
        let stream = differences(&|trace| trace.segments[0].ops = 3);
        assert_eq!(stream.len(), 1);
        assert!(stream[0].starts_with("the operation stream of documents changed: 3 operations"));
        let stream = differences(&|trace| trace.segments[1].stream = "f".repeat(64));
        assert!(stream[0].starts_with("the operation stream of documents-reference changed"));
        assert_eq!(
            differences(&|trace| {
                trace.segments[0].entries.pop();
            }),
            ["documents is laid out in 2 entries, recorded in 3"]
        );
        assert_eq!(
            differences(&|trace| {
                trace.segments[0].entries[0] = Entry::Step("c0001r9999".to_string());
                trace.segments[0].entries[2] = Entry::Step("c9999v0004q0005pffff".to_string());
                trace.digest = "f".repeat(64);
            }),
            [
                "step 0: documents: root hash differ: c0001r9999, recorded c0001r0002",
                "step 1: documents: cost and proof differ: c9999v0004q0005pffff, recorded \
                 c0003v0004q0005p0006",
                &format!(
                    "the trace has 1500 steps with digest {}, recorded 1500 with {DIGEST}",
                    "f".repeat(64)
                ),
            ]
        );
        assert_eq!(
            differences(&|trace| {
                let Entry::Detail(detail) = &mut trace.segments[0].entries[1] else {
                    panic!("the sample has a step in full");
                };
                detail.lines[1] = "root 01".to_string();
                detail.lines.push("proof 1 00".to_string());
            }),
            [
                "step 0: documents: the step in full differs\n      recorded: root 00\n      \
              replayed: root 01\n      replayed: proof 1 00"
            ]
        );
        assert_eq!(
            differences(&|trace| {
                trace.segments[1].entries[0] = Entry::Checkpoint(1000, "ffffffff".to_string());
            }),
            [
                "step 2: documents-reference: a step from 2 to 999 differs: the digest there is \
              ffffffff, recorded 0a0b0c0d"
            ]
        );
        assert_eq!(
            differences(&|trace| {
                trace.segments[1].entries[1] = Entry::Checkpoint(1500, "ffffffff".to_string());
            }),
            [
                "step 1000: documents-reference: a step from 1000 to 1499 differs: the digest \
              there is ffffffff, recorded 1a1b1c1d"
            ]
        );
        for change in [
            Entry::Checkpoint(999, "0a0b0c0d".to_string()),
            Entry::Step("c0001".to_string()),
            Entry::Detail(Detail {
                step: 7,
                lines: Vec::new(),
                remarks: Vec::new(),
            }),
        ] {
            assert_eq!(
                differences(&|trace| trace.segments[1].entries[0] = change.clone()),
                ["step 2: documents-reference: the trace is laid out differently from here"]
            );
        }
        assert_eq!(
            differences(&|trace| trace.steps = 7).len(),
            1,
            "the number of steps is compared"
        );
        assert_eq!(
            differences(&|trace| trace.tally.unverified = 0),
            [
                "the tally is failed=1 unproved=2 unverified=0, recorded failed=1 unproved=2 \
              unverified=3"
            ]
        );
        assert!(!recorded.tally.is_clean());
        assert!(Tally::default().is_clean());
    }

    #[test]
    fn a_long_line_is_cut_in_a_report() {
        assert_eq!(shortened("short"), "short");
        let long = "é".repeat(250);
        let cut = shortened(&long);
        assert!(cut.starts_with(&"é".repeat(200)));
        assert!(cut.ends_with("… (250 characters)"), "{cut}");
        assert_eq!(
            line_differences(&["same".to_string()], &["same".to_string()]),
            ""
        );
        assert_eq!(
            line_differences(&["gone".to_string()], &[]),
            "      recorded: gone"
        );
    }
}
