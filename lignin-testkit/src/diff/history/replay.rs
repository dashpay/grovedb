//! A recorded stream applied to the two engines side by side.

use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    io::{BufRead, BufReader, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use grovedb::GroveDb;
use grovedb_version::version::GroveVersion;

use super::{
    calls::{
        apply, grove_version, held_against_the_recording, path_query, Answer, Applied, Held,
        Transactions,
    },
    stream::{clean, line, same_cost, shorten, value_text, variant, Call, Line},
};
use crate::diff::{
    close,
    engines::{Candidate, Reference},
    pair::catching,
};

/// How a stream is replayed.
#[derive(Clone, Debug, Default)]
pub struct Settings {
    /// The recorded stream.
    pub stream: PathBuf,
    /// Where the two databases are made, in a new directory each.
    pub scratch: PathBuf,
    /// The last block to replay. `None`: to the end of the stream.
    pub stop_height: Option<u64>,
    /// Ranges of heights, both ends included, of which every call is
    /// printed with what both engines answered.
    pub show_heights: Vec<(u64, u64)>,
    /// Paths, as the recorder writes one: every call that names such a path
    /// or one below it is printed with what both engines answered.
    pub watch: Vec<String>,
    /// How many calls the engines may answer differently before the replay
    /// ends. At least one: the replay ends at the first.
    pub keep_going: usize,
    /// A line of progress after every this many blocks. `0`: none.
    pub progress_every: u64,
}

/// The calls of one method: how the engines answered them, and how the
/// answers of the reference compare with the recording node's.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AgainstRecording {
    /// Calls made on both engines.
    pub calls: u64,
    /// Calls the two engines answered alike, in outcome and cost.
    pub alike: u64,
    /// Calls made with a stand-in for an argument the stream does not have.
    pub stand_in: u64,
    /// Why, for the first of them.
    pub stand_in_for: Option<&'static str>,
    /// Calls whose outcome on the reference is not the recording node's.
    pub other_outcome: u64,
    /// Calls whose outcome was not held against the recording node's.
    pub outcome_not_held: u64,
    /// Calls whose cost on the reference is not the recording node's.
    pub other_cost: u64,
    /// Calls whose cost was not held against the recording node's.
    pub cost_not_held: u64,
    /// The first call that is not as recorded: its height and its number in
    /// the stream.
    pub first_other: Option<(u64, u64, String)>,
}

/// What a replay came to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// Blocks whose `finalized` mark was passed.
    pub blocks: u64,
    /// The last such block.
    pub last_height: Option<u64>,
    /// Calls read, the steps of transactions among them.
    pub calls: u64,
    /// Calls made on both engines.
    pub replayed: u64,
    /// Of those, the calls made with a stand-in for an argument.
    pub stand_ins: u64,
    /// By method.
    pub by_method: BTreeMap<String, AgainstRecording>,
    /// Reads that were not made, by method: how many and why (the first).
    pub not_replayed: BTreeMap<String, (u64, String)>,
    /// Calls with a callback that was asked something else than on the
    /// recording node, on the reference.
    pub callbacks_asked_otherwise: u64,
    /// Blocks after which both engines gave a root hash and the two are
    /// equal.
    pub roots_alike: u64,
    /// Of those, the blocks whose root hash is the GroveDB root hash the
    /// recording node had.
    pub roots_as_recorded: u64,
    /// The heights where it is not.
    pub roots_not_as_recorded: Vec<u64>,
    /// The blocks whose mark has no root hash of the recording node.
    pub roots_not_recorded: u64,
    /// Every call, or root hash, that the engines answered differently.
    pub divergences: Vec<String>,
    /// How many calls had been made on both engines when the first of them
    /// was found.
    pub replayed_before_first_divergence: Option<u64>,
    /// Why the replay ended before the end of the stream or the stop
    /// height, if it did.
    pub ended_early: Option<String>,
    /// Calls made after the last block that was finalized.
    pub calls_after_last_block: u64,
    /// Transactions that were open when the replay ended. They were dropped:
    /// what they wrote is in no store that was checked.
    pub open_at_end: usize,
    /// What the check of the candidate's store at the end said.
    pub close: String,
    /// Whether that check found the store sound.
    pub sound: bool,
}

impl Summary {
    /// Whether the engines answered every call that was made alike, gave
    /// equal root hashes after every block, and the candidate's store was
    /// found sound, with the replay not ended early.
    pub fn alike(&self) -> bool {
        self.divergences.is_empty() && self.ended_early.is_none() && self.sound
    }
}

/// Whether two engines answered one call alike: the outcome, the cost, and
/// what their callbacks were asked.
fn alike(of_reference: &Answer, of_candidate: &Answer) -> bool {
    of_reference.outcome == of_candidate.outcome
        && of_reference.cost == of_candidate.cost
        && of_reference.notes == of_candidate.notes
}

/// The root hashes of the two engines after one block: the hash both gave,
/// or how they differ. A hash that an engine did not give differs from
/// every hash, the other engine's failure included.
fn roots(
    of_reference: &Result<String, String>,
    of_candidate: &Result<String, String>,
) -> Result<String, String> {
    match (of_reference, of_candidate) {
        (Ok(reference), Ok(candidate)) if reference == candidate => Ok(reference.clone()),
        _ => Err(format!(
            "reference {}, candidate {}",
            root_text(of_reference),
            root_text(of_candidate)
        )),
    }
}

fn root_text(root: &Result<String, String>) -> String {
    match root {
        Ok(hash) => hash.clone(),
        Err(why) => format!("no hash ({why})"),
    }
}

/// The transactions of each process of the stream that a call was made
/// through, or that were committed or stepped: `(process, number)`.
///
/// GroveDB starts a transaction of its own for a call that is given none,
/// and the recorder numbers that one too. Starting it here as well would put
/// a second writer beside the block's transaction that the chain did not
/// have, so only the transactions that the stream uses are started.
fn used_transactions(stream: &Path) -> Result<HashSet<(u64, u64)>, String> {
    let file = File::open(stream).map_err(|e| format!("{}: {e}", stream.display()))?;
    let mut used = HashSet::new();
    let mut process = 0u64;
    for text in BufReader::new(file).lines() {
        let text = text.map_err(|e| format!("{}: {e}", stream.display()))?;
        if text.starts_with("M\t") {
            if text.contains("\trecorder_open") {
                process += 1;
            }
            continue;
        }
        if !text.starts_with("C\t") {
            continue;
        }
        let mut fields = text.split('\t').skip(3);
        let method = fields.next().unwrap_or("");
        if method == "tx_start" || method == "tx_end" {
            continue;
        }
        for field in fields {
            if field == "|" {
                break;
            }
            if let Some(id) = field.strip_prefix("tx=") {
                if let Ok(id) = id.parse() {
                    used.insert((process, id));
                }
                break;
            }
        }
    }
    Ok(used)
}

/// The subtrees a call names: its path, with its key where the key may be a
/// tree, the paths of its batch's operations, and the path of its query.
fn named(call: &Call) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(path) = call.peek("path") {
        paths.push(path.to_string());
        if let Some(key) = call.peek("key") {
            paths.push(format!("{path}/{key}"));
        }
    }
    for (name, value) in &call.arguments {
        if name == "op" || name == "aop" {
            let mut parts = value.split('|');
            if let (Some(path), Some(key)) = (parts.next(), parts.next()) {
                paths.push(path.to_string());
                if let Some(key) = key.strip_prefix('K') {
                    paths.push(format!("{path}/{key}"));
                }
            }
        }
    }
    if let Some(Ok(query)) = call.peek("path_query").map(path_query) {
        let mut text = String::new();
        for segment in &query.path {
            text.push('/');
            text.push_str(&hex::encode(segment));
        }
        paths.push(text);
    }
    paths
}

fn watched(call: &Call, watch: &[String]) -> bool {
    if watch.is_empty() {
        return false;
    }
    named(call).iter().any(|path| {
        watch.iter().any(|watched| {
            path == watched
                || path
                    .strip_prefix(watched.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        })
    })
}

/// The arguments of a call, short enough for a line.
fn shown(call: &Call) -> String {
    let mut text = String::new();
    let mut operations = 0;
    for (name, value) in &call.arguments {
        match name.as_str() {
            "op" | "aop" => {
                operations += 1;
                if operations <= 6 {
                    let mut parts = value.splitn(4, '|');
                    let (path, key, kind) = (
                        parts.next().unwrap_or(""),
                        parts.next().unwrap_or(""),
                        parts.next().unwrap_or(""),
                    );
                    text.push_str(&format!(" {name}=[{kind} {} {}]", ends(path), ends(key)));
                }
            }
            "uf" | "sr" | "ao" | "element" | "value" | "path_query" => {}
            "path" | "key" => text.push_str(&format!(" {name}={}", ends(value))),
            _ => text.push_str(&format!(" {name}={}", shorten(value))),
        }
    }
    if operations > 6 {
        text.push_str(&format!(" (+{} operations)", operations - 6));
    }
    text
}

/// A long hex text by its two ends.
fn ends(text: &str) -> String {
    if text.len() <= 72 {
        text.to_string()
    } else {
        format!("{}…{}", &text[..36], &text[text.len() - 28..])
    }
}

fn heights(mark: &str, name: &str) -> Option<u64> {
    mark.split(' ')
        .find_map(|word| word.strip_prefix(name))
        .and_then(|value| value.parse().ok())
}

fn word<'a>(mark: &'a str, name: &str) -> Option<&'a str> {
    mark.split(' ').find_map(|word| word.strip_prefix(name))
}

/// Replays the stream on a fresh database of each engine and writes what it
/// finds to `out` as it goes.
///
/// Call it on a thread with a stack of
/// [`STACK_BYTES`](crate::golden::run::STACK_BYTES).
pub fn replay(settings: &Settings, out: &mut dyn Write) -> Result<Summary, String> {
    let io = |e: std::io::Error| format!("writing the report: {e}");
    let used = used_transactions(&settings.stream)?;
    let dir = tempfile::Builder::new()
        .prefix("lignin-history-")
        .tempdir_in(&settings.scratch)
        .map_err(|e| format!("{}: {e}", settings.scratch.display()))?;
    let reference: GroveDb<Reference> = GroveDb::open_with_engine(dir.path().join("reference"))
        .map_err(|e| format!("opening the reference: {e:?}"))?;
    let candidate: GroveDb<Candidate> = GroveDb::open_with_engine(dir.path().join("candidate"))
        .map_err(|e| format!("opening the candidate: {e:?}"))?;
    let mut reference_transactions = Transactions::default();
    let mut candidate_transactions = Transactions::default();

    let mut summary = Summary::default();
    let mut process = 0u64;
    let mut thread: Option<String> = None;
    // The aux keys a call wrote a stand-in under, whatever became of the
    // call's transaction and of the key since.
    let mut stand_in_keys: HashSet<String> = HashSet::new();
    let mut height = 0u64;
    let mut request = String::new();
    let mut ending: Option<u64> = None;
    let mut first_version: Option<&'static GroveVersion> = None;
    let mut version: Option<&'static GroveVersion> = None;
    let started = Instant::now();
    let keep_going = settings.keep_going.max(1);
    let showing = |height: u64| {
        settings
            .show_heights
            .iter()
            .any(|(from, to)| (*from..=*to).contains(&height))
    };

    let file =
        File::open(&settings.stream).map_err(|e| format!("{}: {e}", settings.stream.display()))?;
    'stream: for (number, text) in BufReader::new(file).lines().enumerate() {
        let text = text.map_err(|e| format!("{}: {e}", settings.stream.display()))?;
        let call = match line(&text) {
            Line::Keys => continue,
            Line::Malformed(why) => {
                summary.ended_early = Some(format!("line {} of the stream: {why}", number + 1));
                break 'stream;
            }
            Line::Mark(mark) => {
                if mark.starts_with("recorder_open") {
                    process += 1;
                    thread = None;
                    reference_transactions.end_all();
                    candidate_transactions.end_all();
                    ending = None;
                } else if let Some(rest) = mark.strip_prefix("request ") {
                    request = rest.split(' ').next().unwrap_or("").to_string();
                    if let Some(at) = heights(&mark, "height=") {
                        height = at;
                    }
                } else if mark.starts_with("finalized ") {
                    let at = heights(&mark, "height=").unwrap_or(height);
                    let Some(version) = version else {
                        summary.ended_early = Some(format!(
                            "height {at} was finalized before any call named a grove version: \
                             its root hashes cannot be read"
                        ));
                        break 'stream;
                    };
                    summary.blocks += 1;
                    summary.last_height = Some(at);
                    summary.calls_after_last_block = 0;
                    let read = |root: Result<Result<[u8; 32], String>, String>| match root {
                        Ok(root) => root.map(hex::encode),
                        Err(panic) => Err(format!("panic: {}", shorten(&clean(&panic)))),
                    };
                    let failed = |error: grovedb::Error| format!("error {error:?}");
                    let of_reference = read(catching(|| {
                        reference.root_hash(None, version).value.map_err(failed)
                    }));
                    let of_candidate = read(catching(|| {
                        candidate.root_hash(None, version).value.map_err(failed)
                    }));
                    let recorded = word(&mark, "grove_root=").filter(|root| *root != "-");
                    match roots(&of_reference, &of_candidate) {
                        Ok(root) => {
                            summary.roots_alike += 1;
                            match recorded {
                                None => summary.roots_not_recorded += 1,
                                Some(recorded) if recorded == root => {
                                    summary.roots_as_recorded += 1
                                }
                                Some(_) => summary.roots_not_as_recorded.push(at),
                            }
                        }
                        Err(how) => {
                            let divergence =
                                format!("height {at}: the root hash after the block: {how}");
                            writeln!(out, "DIFFERS {divergence}").map_err(io)?;
                            summary
                                .replayed_before_first_divergence
                                .get_or_insert(summary.replayed);
                            summary.divergences.push(divergence);
                        }
                    }
                    if showing(at) {
                        writeln!(
                            out,
                            "height {at} finalized: root hash reference {}, candidate {}, \
                             recorded {}; app hash of the chain {}",
                            root_text(&of_reference),
                            root_text(&of_candidate),
                            recorded.unwrap_or("-"),
                            word(&mark, "chain_app_hash=").unwrap_or("?")
                        )
                        .map_err(io)?;
                    }
                    if settings.progress_every > 0 && summary.blocks % settings.progress_every == 0
                    {
                        writeln!(
                            out,
                            "progress: height {at}, {} calls replayed, {} divergences, {} s",
                            summary.replayed,
                            summary.divergences.len(),
                            started.elapsed().as_secs()
                        )
                        .map_err(io)?;
                    }
                    if summary.divergences.len() >= keep_going {
                        summary.ended_early = Some(format!("the engines differ, at height {at}"));
                        break 'stream;
                    }
                    if settings.stop_height.is_some_and(|stop| at >= stop) {
                        break 'stream;
                    }
                }
                continue;
            }
            Line::Call(call) => call,
        };
        summary.calls += 1;
        let place = format!(
            "height {height} ({request}), call {} `{}`",
            call.seq, call.method
        );
        // The stream has the order in which calls returned. Of two threads,
        // or of a call made inside a callback of another, that is not the
        // order of what they did to the database.
        match &thread {
            None => thread = Some(call.thread.clone()),
            Some(first) if *first != call.thread => {
                summary.ended_early = Some(format!(
                    "{place} is of thread {}, the calls before it of thread {first}: the order \
                     of what two threads did is not in the stream",
                    call.thread
                ));
                break 'stream;
            }
            Some(_) => {}
        }
        if call.from_callback {
            summary.ended_early = Some(format!(
                "{place} was made inside a callback of another call: where in that call it ran \
                 is not in the stream"
            ));
            break 'stream;
        }
        let transaction = call.peek("tx").and_then(|tx| tx.parse::<u64>().ok());

        // A commit consumes its transaction, so the end of a committed
        // transaction is written just before the line of its commit.
        if let Some(gone) = ending.take()
            && !(call.method == "commit_transaction" && transaction == Some(gone))
        {
            reference_transactions.end(gone);
            candidate_transactions.end(gone);
        }
        // The start and the end of a transaction: its number, for a start
        // its kind, and nothing else.
        if matches!(call.method.as_str(), "tx_start" | "tx_end") {
            let starts = call.method == "tx_start";
            let id = call.transaction();
            let kind = if starts { call.argument("kind") } else { None };
            let wrong = match (id, call.unread()) {
                (None, _) => Some("names no transaction".to_string()),
                (_, Some(name)) => Some(format!(
                    "has an argument `{name}` that this replay does not read"
                )),
                _ if starts && !matches!(kind, Some("write" | "snapshot")) => Some(format!(
                    "is of kind {kind:?}, which is neither `write` nor `snapshot`"
                )),
                _ => None,
            };
            if let Some(wrong) = wrong {
                summary.ended_early = Some(format!("{place} {wrong}"));
                break 'stream;
            }
            if !starts {
                ending = id;
            } else if let Some(id) = id
                && used.contains(&(process, id))
            {
                let snapshot = kind == Some("snapshot");
                reference_transactions.start(&reference, id, snapshot);
                candidate_transactions.start(&candidate, id, snapshot);
            }
            continue;
        }
        if let Some(protocol) = call.peek("gv").and_then(|gv| gv.parse::<u32>().ok()) {
            let of_call = grove_version(protocol)?;
            if first_version.is_none() {
                first_version = Some(of_call);
                close::opened(&candidate, of_call);
            }
            version = Some(of_call);
        }

        let on_reference = catching(|| apply(&reference, &mut reference_transactions, &call));
        let on_candidate = catching(|| apply(&candidate, &mut candidate_transactions, &call));
        let (on_reference, on_candidate) = match (on_reference, on_candidate) {
            (Ok(Ok(r)), Ok(Ok(c))) => (r, c),
            (Ok(Err(why)), _) | (_, Ok(Err(why))) => {
                summary.ended_early = Some(format!("{place} cannot be made: {why}"));
                break 'stream;
            }
            (r, c) => {
                let panic = |side: &Result<_, String>| match side {
                    Err(panic) => format!("panicked: {}", shorten(&clean(panic))),
                    Ok(_) => "answered".to_string(),
                };
                let divergence =
                    format!("{place}: reference {}, candidate {}", panic(&r), panic(&c));
                writeln!(out, "DIFFERS {divergence}").map_err(io)?;
                summary
                    .replayed_before_first_divergence
                    .get_or_insert(summary.replayed);
                summary.divergences.push(divergence);
                summary.ended_early = Some(format!("an engine panicked, at {place}"));
                break 'stream;
            }
        };
        let (of_reference, of_candidate) = match (on_reference, on_candidate) {
            (Applied::Answered(r), Applied::Answered(c)) => (r, c),
            (Applied::NotReplayed(why), _) | (_, Applied::NotReplayed(why)) => {
                let entry = summary
                    .not_replayed
                    .entry(call.method.clone())
                    .or_insert((0, why));
                entry.0 += 1;
                continue;
            }
        };
        summary.replayed += 1;
        summary.calls_after_last_block += 1;
        let entry = summary.by_method.entry(call.method.clone()).or_default();
        entry.calls += 1;
        let alike = alike(&of_reference, &of_candidate);
        if alike {
            entry.alike += 1;
        }
        if let Some(what) = of_reference.stand_in {
            entry.stand_in += 1;
            entry.stand_in_for.get_or_insert(what);
            summary.stand_ins += 1;
            if let ("put_aux", Some(key)) = (call.method.as_str(), call.peek("key")) {
                stand_in_keys.insert(key.to_string());
            }
        }
        let held = match held_against_the_recording(&call.method) {
            Held::Cost if !reads_a_stand_in(&call, &stand_in_keys) => Held::OutcomeAndCost,
            held => held,
        };
        let (outcome, cost) = against_recording(&call, &of_reference, held);
        match outcome {
            Compared::Same => {}
            Compared::Other => entry.other_outcome += 1,
            Compared::NotHeld => entry.outcome_not_held += 1,
        }
        match cost {
            Compared::Same => {}
            Compared::Other => entry.other_cost += 1,
            Compared::NotHeld => entry.cost_not_held += 1,
        }
        if (outcome == Compared::Other || cost == Compared::Other) && entry.first_other.is_none() {
            entry.first_other = Some((
                height,
                call.seq,
                format!(
                    "reference {}{}; recorded {} {} cost {}",
                    of_reference.line(),
                    match &of_reference.outcome {
                        Ok(value) if outcome == Compared::Other => {
                            format!(
                                " (its value as the recorder writes one: {})",
                                value_text(value)
                            )
                        }
                        _ => String::new(),
                    },
                    if call.recorded_ok() { "ok" } else { "error" },
                    shorten(
                        call.recorded_value()
                            .or(call.recorded_error())
                            .unwrap_or("")
                    ),
                    call.recorded_cost().unwrap_or("-"),
                ),
            ));
        }
        if !of_reference.notes.is_empty() {
            summary.callbacks_asked_otherwise += 1;
        }
        if !alike || showing(height) || watched(&call, &settings.watch) {
            writeln!(
                out,
                "{}{place}{}\n    reference: {}\n    candidate: {}\n    recorded:  {} {} cost {}",
                if alike { "" } else { "DIFFERS " },
                shown(&call),
                with_notes(&of_reference),
                with_notes(&of_candidate),
                if call.recorded_ok() { "ok" } else { "error" },
                shorten(
                    call.recorded_value()
                        .or(call.recorded_error())
                        .unwrap_or("")
                ),
                call.recorded_cost().unwrap_or("-"),
            )
            .map_err(io)?;
        }
        if !alike {
            summary
                .replayed_before_first_divergence
                .get_or_insert(summary.replayed - 1);
            summary.divergences.push(format!(
                "{place}: reference {}; candidate {}",
                with_notes(&of_reference),
                with_notes(&of_candidate)
            ));
            if summary.divergences.len() >= keep_going {
                summary.ended_early = Some(format!("the engines differ, at {place}"));
                break 'stream;
            }
        }
    }

    summary.open_at_end = reference_transactions.open();
    // The store is read by the rules of the first grove version of the
    // stream. What a delete on an early version left in it is still there
    // when later calls are made on a later version, and of the versions a
    // stream went through the first allows the most.
    reference_transactions.end_all();
    candidate_transactions.end_all();
    match first_version {
        None => summary.close = "no call with a grove version: nothing to check".to_string(),
        Some(version) => match catching(|| close::check(&reference, &candidate, version)) {
            Ok(Ok(closes)) => {
                summary.sound = true;
                summary.close = closes
                    .line()
                    .unwrap_or_else(|| "the candidate is not LigninDB's engine".to_string());
            }
            Ok(Err(wanting)) => {
                summary.close = format!(
                    "reference: {}; candidate: {}",
                    wanting.reference.join("; "),
                    wanting.candidate.join("; ")
                );
            }
            Err(panic) => summary.close = format!("the check panicked: {}", clean(&panic)),
        },
    }
    Ok(summary)
}

fn with_notes(answer: &Answer) -> String {
    if answer.notes.is_empty() {
        answer.line()
    } else {
        format!("{} [{}]", answer.line(), answer.notes.join("; "))
    }
}

/// Whether a read of aux, by its key or by a prefix of keys, may return a
/// value that was written as a stand-in.
fn reads_a_stand_in(call: &Call, stand_in_keys: &HashSet<String>) -> bool {
    match (call.peek("key"), call.peek("key_prefix")) {
        (Some(key), _) => stand_in_keys.contains(key),
        (None, Some(prefix)) => stand_in_keys.iter().any(|key| key.starts_with(prefix)),
        (None, None) => true,
    }
}

/// How a part of the reference's answer compares with the recording node's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Compared {
    Same,
    Other,
    /// It was not held against the recording: the method is one whose
    /// answer is not, or the recording has no value to hold it against.
    NotHeld,
}

fn same(same: bool) -> Compared {
    if same {
        Compared::Same
    } else {
        Compared::Other
    }
}

/// How the reference's outcome, and its cost, compare with the recording
/// node's. Two values are the same by their `Debug` text, two errors by the
/// variant their `Debug` text begins with, two costs by their values.
fn against_recording(call: &Call, of_reference: &Answer, held: Held) -> (Compared, Compared) {
    let outcome = match (&of_reference.outcome, call.recorded_ok()) {
        _ if held != Held::OutcomeAndCost => Compared::NotHeld,
        (Ok(value), true) => match call.recorded_value() {
            // `?` is a value of a type the recorder does not write.
            None | Some("?") => Compared::NotHeld,
            Some(recorded) => same(recorded == value_text(value)),
        },
        (Err(_), false) => match (
            call.recorded_error().map(variant),
            of_reference.error.as_deref().map(variant),
        ) {
            (Some(recorded), Some(now)) if !recorded.is_empty() => same(recorded == now),
            _ => Compared::NotHeld,
        },
        _ => Compared::Other,
    };
    let cost = match (&of_reference.cost, call.recorded_cost()) {
        _ if held == Held::Neither => Compared::NotHeld,
        (Some(cost), Some(recorded)) => same(same_cost(recorded, cost)),
        (None, None) => Compared::Same,
        _ => Compared::Other,
    };
    (outcome, cost)
}

/// Writes a summary for a reader.
pub fn report(summary: &Summary, out: &mut dyn Write) -> std::io::Result<()> {
    writeln!(
        out,
        "\n{} blocks (last height {:?}), {} calls read, {} made on both engines: {} as the stream \
         has them, {} with a stand-in for an argument",
        summary.blocks,
        summary.last_height,
        summary.calls,
        summary.replayed,
        summary.replayed - summary.stand_ins,
        summary.stand_ins
    )?;
    writeln!(
        out,
        "root hash after a block: both engines gave one and the same at {} of {}; of those it is \
         the recording node's at {}, the mark has none at {}, it is another at heights {:?}",
        summary.roots_alike,
        summary.blocks,
        summary.roots_as_recorded,
        summary.roots_not_recorded,
        &summary.roots_not_as_recorded[..summary.roots_not_as_recorded.len().min(20)]
    )?;
    writeln!(
        out,
        "calls with a callback asked otherwise than on the recording node: {}",
        summary.callbacks_asked_otherwise
    )?;
    writeln!(
        out,
        "method | calls | engines alike | with a stand-in | outcome not the recording's | outcome \
         not held against it | cost not the recording's | cost not held against it | first not \
         as recorded"
    )?;
    for (method, entry) in &summary.by_method {
        writeln!(
            out,
            "{method} | {} | {} | {} | {} | {} | {} | {} | {}",
            entry.calls,
            entry.alike,
            entry.stand_in,
            entry.other_outcome,
            entry.outcome_not_held,
            entry.other_cost,
            entry.cost_not_held,
            entry
                .first_other
                .as_ref()
                .map_or(String::new(), |(height, seq, what)| format!(
                    "height {height} call {seq}: {what}"
                ))
        )?;
    }
    for (method, entry) in &summary.by_method {
        if let Some(what) = entry.stand_in_for {
            writeln!(out, "stand-in: {method} x{}: {what}", entry.stand_in)?;
        }
    }
    for (method, (count, why)) in &summary.not_replayed {
        writeln!(out, "not made: {method} x{count}: {why}")?;
    }
    writeln!(out, "divergences: {}", summary.divergences.len())?;
    for divergence in &summary.divergences {
        writeln!(out, "  {divergence}")?;
    }
    if let Some(before) = summary.replayed_before_first_divergence {
        writeln!(
            out,
            "calls made before the first divergence: {before}; the calls after it were made on \
             states that may differ, and the counts above have them"
        )?;
    }
    if let Some(why) = &summary.ended_early {
        writeln!(out, "ended early: {why}")?;
    }
    if summary.calls_after_last_block > 0 || summary.open_at_end > 0 {
        writeln!(
            out,
            "after the last finalized block: {} calls made, {} transactions open at the end and \
             dropped; what an open transaction wrote is in no store that was checked",
            summary.calls_after_last_block, summary.open_at_end
        )?;
    }
    writeln!(out, "the candidate's store at the end: {}", summary.close)?;
    writeln!(
        out,
        "{}",
        if summary.alike() {
            "ALIKE"
        } else {
            "NOT ALIKE"
        }
    )
}

#[cfg(test)]
mod tests {
    use grovedb_costs::OperationCost;

    use super::*;

    fn answered(outcome: Result<&str, &str>, seeks: u32) -> Answer {
        Answer {
            outcome: outcome.map(str::to_string).map_err(str::to_string),
            error: outcome.err().map(|name| format!("{name}(\"why\")")),
            cost: Some(OperationCost::with_seek_count(seeks)),
            notes: Vec::new(),
            stand_in: None,
        }
    }

    fn call(text: &str) -> Call {
        match line(text) {
            Line::Call(call) => call,
            other => panic!("a call line, not {other:?}"),
        }
    }

    #[test]
    fn two_answers_are_alike_in_outcome_cost_and_callbacks_each() {
        let one = answered(Ok("()"), 3);
        assert!(alike(&one, &answered(Ok("()"), 3)));
        assert!(!alike(&one, &answered(Ok("true"), 3)), "the outcome only");
        assert!(!alike(&one, &answered(Err("PathKeyNotFound"), 3)));
        assert!(!alike(&one, &answered(Ok("()"), 4)), "the cost only");
        let mut no_cost = answered(Ok("()"), 3);
        no_cost.cost = None;
        assert!(!alike(&one, &no_cost), "a cost and none");
        let mut noted = answered(Ok("()"), 3);
        noted
            .notes
            .push("the flag-merge callback was asked once more".into());
        assert!(!alike(&one, &noted), "what a callback was asked only");
        assert!(
            !alike(
                &answered(Err("PathKeyNotFound"), 3),
                &answered(Err("InvalidPath"), 3)
            ),
            "the error only"
        );
    }

    #[test]
    fn root_hashes_are_alike_only_where_both_engines_gave_one() {
        let hash = |text: &str| Ok::<String, String>(text.to_string());
        let failed = || Err::<String, String>("error CorruptedData".to_string());
        assert_eq!(roots(&hash("aa"), &hash("aa")), Ok("aa".to_string()));
        assert!(roots(&hash("aa"), &hash("ab")).is_err());
        assert!(roots(&hash("aa"), &failed()).is_err());
        assert!(
            roots(&failed(), &failed()).is_err(),
            "two engines that failed alike gave no hash"
        );
    }

    #[test]
    fn the_reference_is_held_against_the_recording_by_value_error_variant_and_cost() {
        let get = "C\t4\t1\tget\tpath=\tkey=61\ttx=-\tgv=1\t|\t";
        let cost = "c=3,0,0,0,NoStorageRemoval,0,0";
        let held = Held::OutcomeAndCost;
        let recorded = call(&format!("{get}ok\tv=()\t{cost}"));
        assert_eq!(
            against_recording(&recorded, &answered(Ok("()"), 3), held),
            (Compared::Same, Compared::Same)
        );
        assert_eq!(
            against_recording(&recorded, &answered(Ok("true"), 4), held),
            (Compared::Other, Compared::Other)
        );
        assert_eq!(
            against_recording(&recorded, &answered(Err("PathKeyNotFound"), 3), held),
            (Compared::Other, Compared::Same)
        );
        assert_eq!(
            against_recording(&recorded, &answered(Ok("()"), 3), Held::Cost),
            (Compared::NotHeld, Compared::Same)
        );
        assert_eq!(
            against_recording(&recorded, &answered(Ok("true"), 4), Held::Neither),
            (Compared::NotHeld, Compared::NotHeld)
        );

        let failed = call(&format!("{get}err=InvalidPath(\"no\")\t{cost}"));
        assert_eq!(
            against_recording(&failed, &answered(Err("InvalidPath"), 3), held).0,
            Compared::Same
        );
        assert_eq!(
            against_recording(&failed, &answered(Err("PathKeyNotFound"), 3), held).0,
            Compared::Other,
            "two errors of two variants"
        );

        let opaque = call(&format!("{get}ok\tv=?"));
        assert_eq!(
            against_recording(&opaque, &answered(Ok("()"), 3), held),
            (Compared::NotHeld, Compared::Other),
            "a value the recorder does not write, and a cost where the recording has none"
        );
        let mut no_cost = answered(Ok("()"), 3);
        no_cost.cost = None;
        assert_eq!(
            against_recording(&call(&format!("{get}ok\tv=()")), &no_cost, held),
            (Compared::Same, Compared::Same)
        );
    }
}
