//! The recorded stream as text: its lines and their fields.
//!
//! The recorder writes one line for every outermost call of a public method
//! of `GroveDb`, tab-separated:
//!
//! ```text
//! C <seq> <thread> <method> <name=value>... | <result fields>...
//! M <seq> <thread> <text>
//! S <seq> <thread> write tx=<n> put=<cf>:<key>:<len> del=<cf>:<key> ...
//! ```
//!
//! `M` is a mark of the recorder's caller: a request with its height, a
//! block that was finalized with its hashes, the start of a process. `S` is
//! the keys of one write batch, where a run asked for them. Bytes are
//! lower-case hex. A path is its segments, each preceded by `/`; the root is
//! the empty value.
//!
//! A line that is none of these, or a call line that is not whole (no
//! number, no `|` and result, an argument with no `=`, a transaction or a
//! grove version that is no number), is [`Line::Malformed`]: a replay ends
//! there.

use std::cell::RefCell;

use grovedb_costs::{
    storage_cost::{
        removal::{StorageRemovalPerEpochByIdentifier, StorageRemovedBytes},
        StorageCost,
    },
    OperationCost,
};

/// One call, as the recorder wrote it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    /// The line's number in the recorder's count. Calls are numbered at
    /// their return.
    pub seq: u64,
    /// The recorder's number of the thread that made the call.
    pub thread: String,
    /// The public method.
    pub method: String,
    /// Whether the call was made from inside a callback of another call.
    pub from_callback: bool,
    /// The argument fields, in the order they were written.
    pub arguments: Vec<(String, String)>,
    /// The result fields: `ok` or `err=…`, `v=…`, `c=…`.
    pub result: Vec<String>,
    /// Which arguments were read ([`Call::unread`]).
    read: RefCell<Vec<bool>>,
}

/// One line of the stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Line {
    /// A mark of the recorder's caller or of the recorder.
    Mark(String),
    /// A call.
    Call(Call),
    /// The keys of a write batch, which a replay does not use.
    Keys,
    /// A line that is no whole record, and why.
    Malformed(String),
}

/// The arguments a call may have several of: the operations of a batch and
/// what its callbacks were asked.
const REPEATED: &[&str] = &["op", "aop", "uf", "sr", "ao"];

/// Reads one line.
pub fn line(text: &str) -> Line {
    let text = text.trim_end_matches(['\n', '\r']);
    let malformed = |why: &str| Line::Malformed(format!("{why}: `{}`", shorten(text)));
    let mut fields = text.split('\t');
    let (Some(kind), Some(seq), Some(thread), Some(first)) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
        return malformed("a line of fewer than four fields");
    };
    let Ok(seq) = seq.parse::<u64>() else {
        return malformed("a line whose number is none");
    };
    match kind {
        "M" => {
            let mut mark = first.to_string();
            for more in fields {
                mark.push(' ');
                mark.push_str(more);
            }
            Line::Mark(mark)
        }
        "S" => Line::Keys,
        "C" => {
            let mut call = Call {
                seq,
                thread: thread.to_string(),
                method: first.to_string(),
                from_callback: false,
                arguments: Vec::new(),
                result: Vec::new(),
                read: RefCell::new(Vec::new()),
            };
            let mut in_result = false;
            for field in fields {
                if in_result {
                    call.result.push(field.to_string());
                } else if field == "|" {
                    in_result = true;
                } else if field == "cb" && call.arguments.is_empty() {
                    call.from_callback = true;
                } else {
                    let Some((name, value)) = field.split_once('=') else {
                        return malformed("an argument with no `=`");
                    };
                    if !REPEATED.contains(&name) && call.peek(name).is_some() {
                        return malformed("an argument written twice");
                    }
                    match name {
                        "tx" if value != "-" && value.parse::<u64>().is_err() => {
                            return malformed("a transaction that is no number");
                        }
                        "gv" if value.parse::<u32>().is_err() => {
                            return malformed("a grove version that is no number");
                        }
                        _ => {}
                    }
                    call.arguments.push((name.to_string(), value.to_string()));
                }
            }
            match call.result.first() {
                Some(first) if first == "ok" || first.starts_with("err=") => {}
                _ => return malformed("a call with no result"),
            }
            call.read = RefCell::new(vec![false; call.arguments.len()]);
            Line::Call(call)
        }
        _ => malformed("a line of an unknown kind"),
    }
}

impl Call {
    /// The first argument of this name, which is not counted as read.
    pub fn peek(&self, name: &str) -> Option<&str> {
        self.arguments
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// The first argument of this name.
    pub fn argument(&self, name: &str) -> Option<&str> {
        let at = self.arguments.iter().position(|(n, _)| n == name)?;
        if let Some(read) = self.read.borrow_mut().get_mut(at) {
            *read = true;
        }
        Some(self.arguments[at].1.as_str())
    }

    /// The first argument of this name, or an error that names it.
    pub fn required(&self, name: &str) -> Result<&str, String> {
        self.argument(name)
            .ok_or_else(|| format!("`{}` has no argument `{name}`", self.method))
    }

    /// Every argument of one of these names, with its name, in the order
    /// they were written.
    pub fn every(&self, names: &[&str]) -> Vec<(&str, &str)> {
        let mut read = self.read.borrow_mut();
        self.arguments
            .iter()
            .enumerate()
            .filter(|(_, (name, _))| names.contains(&name.as_str()))
            .map(|(at, (name, value))| {
                if let Some(read) = read.get_mut(at) {
                    *read = true;
                }
                (name.as_str(), value.as_str())
            })
            .collect()
    }

    /// The name of the first argument that nothing has read.
    pub fn unread(&self) -> Option<&str> {
        let read = self.read.borrow();
        self.arguments
            .iter()
            .zip(read.iter())
            .find(|(_, read)| !**read)
            .map(|((name, _), _)| name.as_str())
    }

    /// The number of the transaction the call was given, if it was given
    /// one.
    pub fn transaction(&self) -> Option<u64> {
        self.argument("tx").and_then(|tx| tx.parse().ok())
    }

    /// The protocol number of the grove version the call was made with.
    pub fn grove_version(&self) -> Option<u32> {
        self.argument("gv").and_then(|gv| gv.parse().ok())
    }

    /// Whether the call returned no error on the recording node.
    pub fn recorded_ok(&self) -> bool {
        self.result.first().is_some_and(|first| first == "ok")
    }

    /// The error the call returned on the recording node, as its `Debug`
    /// text, of which the recorder kept the first 400 characters.
    pub fn recorded_error(&self) -> Option<&str> {
        self.result_field("err=")
    }

    /// The value the call returned on the recording node: its `Debug` text
    /// where that is short, or `#`, a hash of the text and its length.
    pub fn recorded_value(&self) -> Option<&str> {
        self.result_field("v=")
    }

    /// The cost the call returned on the recording node, as [`cost_text`]
    /// writes one.
    pub fn recorded_cost(&self) -> Option<&str> {
        self.result_field("c=")
    }

    fn result_field(&self, prefix: &str) -> Option<&str> {
        self.result.iter().find_map(|f| f.strip_prefix(prefix))
    }
}

/// The name an error's `Debug` text begins with: its variant.
pub fn variant(debug_text: &str) -> &str {
    let end = debug_text
        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
        .unwrap_or(debug_text.len());
    &debug_text[..end]
}

/// Bytes from their hex.
pub fn bytes(hex_text: &str) -> Result<Vec<u8>, String> {
    hex::decode(hex_text).map_err(|e| format!("not hex ({e}): `{}`", shorten(hex_text)))
}

/// A path from its text: `/aa/bb`, or the empty text for the root.
pub fn path(text: &str) -> Result<Vec<Vec<u8>>, String> {
    if text.is_empty() {
        return Ok(Vec::new());
    }
    let Some(rest) = text.strip_prefix('/') else {
        return Err(format!("not a path: `{}`", shorten(text)));
    };
    rest.split('/').map(bytes).collect()
}

/// The first characters of a long text, for a message.
pub fn shorten(text: &str) -> String {
    const SHOWN: usize = 160;
    match text.char_indices().nth(SHOWN) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

/// A text with no tab and no line break, as the recorder writes one.
pub fn clean(text: &str) -> String {
    text.replace(['\t', '\n', '\r'], " ")
}

/// A cost as the recorder writes it: seeks, loaded bytes, added bytes,
/// replaced bytes, the removed bytes by their `Debug` text with `;` for
/// `,`, hash calls and Sinsemilla hash calls.
pub fn cost_text(cost: &OperationCost) -> String {
    format!(
        "{},{},{},{},{},{},{}",
        cost.seek_count,
        cost.storage_loaded_bytes,
        cost.storage_cost.added_bytes,
        cost.storage_cost.replaced_bytes,
        clean(&format!("{:?}", cost.storage_cost.removed_bytes)).replace(',', ";"),
        cost.hash_node_calls,
        cost.sinsemilla_hash_calls,
    )
}

/// A cost from the text [`cost_text`] writes.
pub fn cost_of(text: &str) -> Result<OperationCost, String> {
    let wrong = || format!("not a cost: `{}`", shorten(text));
    fn number<T: std::str::FromStr>(part: Option<&str>) -> Option<T> {
        part?.parse().ok()
    }
    // Four numbers, the removed bytes, two numbers. The removed bytes have
    // no `,` in them: the recorder writes `;` for it.
    let parts: Vec<&str> = text.split(',').collect();
    let [seeks, loaded, added, replaced, removed, hashes, sinsemilla] = parts.as_slice() else {
        return Err(wrong());
    };
    let seek_count = number(Some(seeks)).ok_or_else(wrong)?;
    let storage_loaded_bytes = number(Some(loaded)).ok_or_else(wrong)?;
    let added_bytes = number(Some(added)).ok_or_else(wrong)?;
    let replaced_bytes = number(Some(replaced)).ok_or_else(wrong)?;
    let hash_node_calls = number(Some(hashes)).ok_or_else(wrong)?;
    let sinsemilla_hash_calls = number(Some(sinsemilla)).ok_or_else(wrong)?;
    let removed_bytes = removed_debug(removed)?;
    Ok(OperationCost {
        seek_count,
        storage_cost: StorageCost {
            added_bytes,
            replaced_bytes,
            removed_bytes,
        },
        storage_loaded_bytes,
        hash_node_calls,
        sinsemilla_hash_calls,
    })
}

/// Removed bytes from their `Debug` text with `;` for `,`, as in a cost.
fn removed_debug(text: &str) -> Result<StorageRemovedBytes, String> {
    let wrong = || format!("not removed bytes: `{}`", shorten(text));
    if text == "NoStorageRemoval" {
        return Ok(StorageRemovedBytes::NoStorageRemoval);
    }
    if let Some(basic) = text
        .strip_prefix("BasicStorageRemoval(")
        .and_then(|rest| rest.strip_suffix(')'))
    {
        return basic
            .parse()
            .map(StorageRemovedBytes::BasicStorageRemoval)
            .map_err(|_| wrong());
    }
    let sectioned = text
        .strip_prefix("SectionedStorageRemoval({")
        .and_then(|rest| rest.strip_suffix("})"))
        .ok_or_else(wrong)?;
    let mut by_identifier = StorageRemovalPerEpochByIdentifier::new();
    if sectioned.is_empty() {
        return Ok(StorageRemovedBytes::SectionedStorageRemoval(by_identifier));
    }
    // `[1; 2; …]: {3: 4; 5: 6}; [7; 8; …]: {}`
    let sectioned = sectioned.strip_suffix('}').ok_or_else(wrong)?;
    for section in sectioned.split("}; ") {
        let (identifier, epochs) = section.split_once("]: {").ok_or_else(wrong)?;
        let identifier: Vec<u8> = identifier
            .strip_prefix('[')
            .ok_or_else(wrong)?
            .split("; ")
            .map(|byte| byte.parse().map_err(|_| wrong()))
            .collect::<Result<_, _>>()?;
        let identifier: [u8; 32] = identifier.try_into().map_err(|_| wrong())?;
        let by_epoch = by_identifier.entry(identifier).or_default();
        for epoch in epochs.split("; ").filter(|epoch| !epoch.is_empty()) {
            let (epoch, removed) = epoch.split_once(": ").ok_or_else(wrong)?;
            by_epoch.insert(
                epoch.parse().map_err(|_| wrong())?,
                removed.parse().map_err(|_| wrong())?,
            );
        }
    }
    Ok(StorageRemovedBytes::SectionedStorageRemoval(by_identifier))
}

/// Whether a cost is the one a text of the recorder names. Two costs with
/// bytes removed by section are the same whatever the order their text
/// lists the sections and epochs in. A text that is not read as a cost is
/// held against the cost's own text.
pub fn same_cost(recorded: &str, cost: &OperationCost) -> bool {
    match cost_of(recorded) {
        Ok(recorded) => recorded == *cost,
        Err(_) => recorded == cost_text(cost),
    }
}

/// The longest `Debug` text of a value that the recorder wrote as it is.
const VALUE_TEXT: usize = 120;

/// A value's `Debug` text in the form the recorder wrote a value in: the
/// text where it is short, else `#`, the first 16 bytes of its Blake3 hash
/// and its length.
pub fn value_text(debug_text: &str) -> String {
    if debug_text.len() <= VALUE_TEXT {
        clean(debug_text)
    } else {
        let hash = blake3::hash(debug_text.as_bytes());
        format!(
            "#{}:{}",
            hex::encode(&hash.as_bytes()[..16]),
            debug_text.len()
        )
    }
}

/// Removed bytes from the recorder's text: `N`, `B<bytes>`, or `S` and,
/// joined by `&`, for each identifier its hex, `:` and its
/// `<epoch>=<bytes>` joined by `+`.
pub fn removed_bytes(text: &str) -> Result<StorageRemovedBytes, String> {
    let wrong = || format!("not removed bytes: `{}`", shorten(text));
    if text == "N" {
        return Ok(StorageRemovedBytes::NoStorageRemoval);
    }
    if let Some(basic) = text.strip_prefix('B') {
        return basic
            .parse()
            .map(StorageRemovedBytes::BasicStorageRemoval)
            .map_err(|_| wrong());
    }
    let sectioned = text.strip_prefix('S').ok_or_else(wrong)?;
    let mut by_identifier = StorageRemovalPerEpochByIdentifier::new();
    for part in sectioned.split('&').filter(|part| !part.is_empty()) {
        let (identifier, epochs) = part.split_once(':').ok_or_else(wrong)?;
        let identifier: [u8; 32] = bytes(identifier)?.try_into().map_err(|_| wrong())?;
        let by_epoch = by_identifier.entry(identifier).or_default();
        for epoch in epochs.split('+').filter(|epoch| !epoch.is_empty()) {
            let (epoch, removed) = epoch.split_once('=').ok_or_else(wrong)?;
            by_epoch.insert(
                epoch.parse().map_err(|_| wrong())?,
                removed.parse().map_err(|_| wrong())?,
            );
        }
    }
    Ok(StorageRemovedBytes::SectionedStorageRemoval(by_identifier))
}

/// The value of a field of a struct's `Debug` text: of `name: value` in
/// `Options { name: value, other: … }`, the `value`.
pub fn debug_field<'a>(debug_text: &'a str, name: &str) -> Option<&'a str> {
    let at = debug_text.find(&format!("{name}: "))? + name.len() + 2;
    let rest = &debug_text[at..];
    let mut depth = 0usize;
    for (index, character) in rest.char_indices() {
        match character {
            '(' | '{' | '[' => depth += 1,
            ')' | '}' | ']' if depth == 0 => return Some(rest[..index].trim()),
            ')' | '}' | ']' => depth -= 1,
            ',' if depth == 0 => return Some(&rest[..index]),
            _ => {}
        }
    }
    Some(rest.trim())
}

/// A `bool` field of a struct's `Debug` text.
pub fn debug_bool(debug_text: &str, name: &str) -> Result<bool, String> {
    match debug_field(debug_text, name) {
        Some("true") => Ok(true),
        Some("false") => Ok(false),
        other => Err(format!(
            "`{name}` is {other:?} in `{}`",
            shorten(debug_text)
        )),
    }
}

/// `Some(inner)` or `None`, from the `Debug` text of an `Option`.
pub fn debug_option(debug_text: &str) -> Result<Option<&str>, String> {
    if debug_text == "None" {
        return Ok(None);
    }
    debug_text
        .strip_prefix("Some(")
        .and_then(|rest| rest.strip_suffix(')'))
        .map(Some)
        .ok_or_else(|| format!("not an Option: `{}`", shorten(debug_text)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_line_is_read_with_its_fields_in_order() {
        let read = line(
            "C\t9\t1\tinsert\tpath=\tkey=40\telement=020000\toptions=None\ttx=3\tgv=1\twas=-@root\t|\tok\tv=()\tc=3,0,109,0,NoStorageRemoval,3,0\n",
        );
        let Line::Call(call) = read else {
            panic!("a call line");
        };
        assert_eq!(call.seq, 9);
        assert_eq!(call.method, "insert");
        assert_eq!(call.argument("path"), Some(""));
        assert_eq!(call.argument("key"), Some("40"));
        assert_eq!(call.transaction(), Some(3));
        assert_eq!(call.grove_version(), Some(1));
        assert!(call.recorded_ok());
        assert_eq!(call.recorded_value(), Some("()"));
        assert_eq!(call.recorded_cost(), Some("3,0,109,0,NoStorageRemoval,3,0"));
        assert_eq!(path("").unwrap(), Vec::<Vec<u8>>::new());
        assert_eq!(path("/30/0100").unwrap(), vec![vec![0x30], vec![1, 0]]);
        assert_eq!(path("/").unwrap(), vec![Vec::<u8>::new()]);
    }

    #[test]
    fn a_call_with_no_transaction_and_an_error_is_read() {
        let Line::Call(call) = line("C\t7\t1\tcommit_transaction\ttx=24\t|\terr=StorageError(RocksDBError(Error { message: \"Resource busy: \" }))\tc=0,0,0,0,NoStorageRemoval,0,0") else {
            panic!("a call line");
        };
        assert!(!call.recorded_ok());
        assert!(call.recorded_error().unwrap().contains("Resource busy"));
        let Line::Call(call) = line("C\t8\t1\troot_hash\ttx=-\tgv=1\t|\tok") else {
            panic!("a call line");
        };
        assert_eq!(call.transaction(), None);
        assert_eq!(
            line("M\t1\t1\trequest info line=1"),
            Line::Mark("request info line=1".into())
        );
        assert_eq!(line("S\t1\t1\twrite\ttx=1"), Line::Keys);
    }

    #[test]
    fn a_line_that_is_no_whole_record_is_told_apart() {
        for text in [
            "",
            "C\t9\t1",
            "C\tx\t1\troot_hash\ttx=-\tgv=1\t|\tok",
            "C\t9\t1\troot_hash\ttx=-\tgv=1",
            "C\t9\t1\troot_hash\ttx=-\tgv=1\t|",
            "C\t9\t1\troot_hash\ttx=-\tgv=1\t|\tv=()",
            "C\t9\t1\tinsert\tpath=\tkey=40\ttx=7x\tgv=1\t|\tok",
            "C\t9\t1\tinsert\tpath=\tkey=40\ttx=7\tgv=one\t|\tok",
            "C\t9\t1\tinsert\tpath=\tkey=40\tkey=41\ttx=7\tgv=1\t|\tok",
            "C\t9\t1\tinsert\tpath=\tkey\ttx=7\tgv=1\t|\tok",
            "X\t9\t1\tsomething",
        ] {
            assert!(
                matches!(line(text), Line::Malformed(_)),
                "`{text}` is read as {:?}",
                line(text)
            );
        }
    }

    #[test]
    fn an_argument_nothing_read_is_named() {
        let Line::Call(call) =
            line("C\t9\t1\tapply_batch\tops=2\top=a\top=b\tnew_option=true\ttx=3\tgv=1\t|\tok")
        else {
            panic!("a call line");
        };
        assert_eq!(call.unread(), Some("ops"));
        assert_eq!(call.peek("ops"), Some("2"));
        assert_eq!(call.unread(), Some("ops"), "a peek is no read");
        assert_eq!(call.argument("ops"), Some("2"));
        assert_eq!(call.every(&["op"]), vec![("op", "a"), ("op", "b")]);
        assert_eq!(call.unread(), Some("new_option"));
        assert_eq!(call.argument("new_option"), Some("true"));
        assert_eq!(
            (call.transaction(), call.grove_version()),
            (Some(3), Some(1))
        );
        assert_eq!(call.unread(), None);
        assert_eq!(variant("InvalidPath(\"no\")"), "InvalidPath");
        assert_eq!(variant("Overflow"), "Overflow");
    }

    #[test]
    fn removed_bytes_and_debug_fields_are_read() {
        assert_eq!(
            removed_bytes("N").unwrap(),
            StorageRemovedBytes::NoStorageRemoval
        );
        assert_eq!(
            removed_bytes("B12").unwrap(),
            StorageRemovedBytes::BasicStorageRemoval(12)
        );
        let identifier = "11".repeat(32);
        let StorageRemovedBytes::SectionedStorageRemoval(map) =
            removed_bytes(&format!("S{identifier}:0=5+3=7")).unwrap()
        else {
            panic!("sectioned");
        };
        let epochs = &map[&[0x11; 32]];
        assert_eq!((epochs.get(0), epochs.get(3)), (Some(&5), Some(&7)));
        assert!(removed_bytes("X").is_err());

        let options = "DeleteUpTreeOptions { allow_deleting_non_empty_trees: false, stop_path_height: Some(4), backwards_references: DontCheck }";
        assert_eq!(
            debug_bool(options, "allow_deleting_non_empty_trees"),
            Ok(false)
        );
        assert_eq!(debug_field(options, "stop_path_height"), Some("Some(4)"));
        assert_eq!(
            debug_field(options, "backwards_references"),
            Some("DontCheck")
        );
        assert_eq!(debug_option("Some(4)"), Ok(Some("4")));
        assert_eq!(debug_option("None"), Ok(None));
    }

    #[test]
    fn a_cost_is_read_back_from_its_text_whatever_the_order_of_its_sections() {
        let mut cost = OperationCost {
            seek_count: 119,
            storage_loaded_bytes: 12365,
            storage_cost: StorageCost {
                added_bytes: 582,
                replaced_bytes: 6035,
                removed_bytes: StorageRemovedBytes::NoStorageRemoval,
            },
            hash_node_calls: 197,
            sinsemilla_hash_calls: 1,
        };
        assert_eq!(cost_of(&cost_text(&cost)).unwrap(), cost);
        cost.storage_cost.removed_bytes = StorageRemovedBytes::BasicStorageRemoval(77);
        assert_eq!(cost_of(&cost_text(&cost)).unwrap(), cost);

        let mut by_identifier = StorageRemovalPerEpochByIdentifier::new();
        by_identifier.entry([162; 32]).or_default().insert(11, 3);
        by_identifier.entry([162; 32]).or_default().insert(15, 589);
        by_identifier.entry([0; 32]).or_default().insert(0, 8626);
        cost.storage_cost.removed_bytes =
            StorageRemovedBytes::SectionedStorageRemoval(by_identifier);
        let text = cost_text(&cost);
        assert_eq!(cost_of(&text).unwrap(), cost, "{text}");

        // The same cost with its epochs listed in the other order.
        let zero = "0; ".repeat(31) + "0";
        let other = "162; ".repeat(31) + "162";
        let listed = format!(
            "119,12365,582,6035,SectionedStorageRemoval({{[{other}]: {{15: 589; 11: 3}}; \
             [{zero}]: {{0: 8626}}}}),197,1"
        );
        assert_ne!(listed, text, "the order of this text is another");
        assert!(same_cost(&listed, &cost));
        assert!(!same_cost(&listed.replace("15: 589", "15: 588"), &cost));
        assert!(!same_cost(&listed.replace(",197,1", ",198,1"), &cost));
        assert!(!same_cost("119,12365,582", &cost), "a text cut short");
        assert!(cost_of("1,2,3,4,SomethingElse,5,6").is_err());

        // No identifier, and an identifier with no epoch.
        let mut none = cost.clone();
        none.storage_cost.removed_bytes =
            StorageRemovedBytes::SectionedStorageRemoval(StorageRemovalPerEpochByIdentifier::new());
        assert_eq!(cost_of(&cost_text(&none)).unwrap(), none);
        let mut by_identifier = StorageRemovalPerEpochByIdentifier::new();
        by_identifier.entry([7; 32]).or_default();
        by_identifier.entry([9; 32]).or_default().insert(2, 5);
        let mut empty = cost.clone();
        empty.storage_cost.removed_bytes =
            StorageRemovedBytes::SectionedStorageRemoval(by_identifier);
        assert_eq!(cost_of(&cost_text(&empty)).unwrap(), empty);
        assert_ne!(cost_of(&cost_text(&empty)).unwrap(), none);
    }

    #[test]
    fn a_long_value_is_written_as_the_recorder_writes_it() {
        assert_eq!(value_text("()"), "()");
        let long = "x".repeat(121);
        let text = value_text(&long);
        assert!(text.starts_with('#') && text.ends_with(":121"), "{text}");
    }
}
