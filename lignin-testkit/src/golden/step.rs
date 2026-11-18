//! One step of a trace: what the engine returned for one operation.
//!
//! A [`Step`] holds everything that is compared: the cost, what was
//! returned, the root hash and, for a query, the proof. It has two
//! renderings. [`Step::line`] is the short form stored for every step, one
//! 16-bit digest per part, so a replay can say which part of which step
//! differs. [`Step::detail`] is the long form, every value written out,
//! stored for a sample of steps and for every step of a full dump.
//! [`TraceDigest`] chains the canonical bytes of every step into the one
//! digest that decides whether two traces are the same.

use std::fmt::Write as _;

use grovedb_costs::{storage_cost::removal::StorageRemovedBytes, OperationCost};

/// What one operation of a trace produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    /// The cost the operation reported, whether or not it succeeded.
    pub cost: OperationCost,
    /// What the operation returned.
    pub outcome: Outcome,
    /// The root hash after the operation, for everything but a read.
    pub root_hash: Option<[u8; 32]>,
    /// The proof of the operation, for a query.
    pub proof: Option<Proof>,
}

/// What an operation returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The value returned.
    Returned(Value),
    /// The operation failed with the error of this name.
    Failed(String),
}

/// A value an operation returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Value {
    /// Nothing: a write, a batch and a commit return no value.
    Nothing,
    /// One element, as its GroveDB serialization.
    Element(Vec<u8>),
    /// The rows of a query, in the order returned.
    Rows(Vec<Row>),
}

/// One row of a query result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Row {
    /// The path of the subtree the row was found in.
    pub path: Vec<Vec<u8>>,
    /// The key of the row.
    pub key: Vec<u8>,
    /// The element, as its GroveDB serialization.
    pub element: Vec<u8>,
}

impl Value {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Value::Nothing => out.push(0),
            Value::Element(element) => {
                out.push(1);
                put_bytes(out, element);
            }
            Value::Rows(rows) => {
                out.push(2);
                put_len(out, rows.len());
                for row in rows {
                    put_len(out, row.path.len());
                    for segment in &row.path {
                        put_bytes(out, segment);
                    }
                    put_bytes(out, &row.key);
                    put_bytes(out, &row.element);
                }
            }
        }
    }

    fn detail(&self, lines: &mut Vec<String>) {
        match self {
            Value::Nothing => {}
            Value::Element(element) => lines.push(format!("element {}", hex::encode(element))),
            Value::Rows(rows) => {
                lines.push(format!("rows {}", rows.len()));
                for row in rows {
                    let path: Vec<String> = row
                        .path
                        .iter()
                        .map(|segment| hex_or_dash(segment))
                        .collect();
                    lines.push(format!(
                        "row {} {} {}",
                        if path.is_empty() {
                            "-".to_string()
                        } else {
                            path.join("/")
                        },
                        hex_or_dash(&row.key),
                        hex::encode(&row.element)
                    ));
                }
            }
        }
    }

    fn payload_len(&self) -> usize {
        match self {
            Value::Nothing => 0,
            Value::Element(element) => element.len(),
            Value::Rows(rows) => rows
                .iter()
                .map(|row| {
                    row.path.iter().map(Vec::len).sum::<usize>() + row.key.len() + row.element.len()
                })
                .sum(),
        }
    }
}

/// Bytes as hex, with `-` standing for none, so a field is never empty.
fn hex_or_dash(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        "-".to_string()
    } else {
        hex::encode(bytes)
    }
}

/// The proof of a query against committed state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Proof {
    /// The cost proving reported, whether or not it succeeded.
    pub cost: OperationCost,
    /// What proving returned.
    pub proved: Proved,
}

/// What proving a query returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Proved {
    /// The proof, and what verifying it against the query gave.
    Bytes(Vec<u8>, Verification),
    /// Proving failed with the error of this name.
    Failed(String),
}

/// What verifying a proof against its query gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verification {
    /// The proof verifies to the root hash of the committed state.
    Verified,
    /// The proof verifies, to another root hash.
    OtherRoot,
    /// The verifier refused the proof with the error of this name.
    Refused(String),
}

/// The part of a step a field of its short line stands for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Part {
    /// The `OperationCost` of the operation.
    Cost,
    /// The value the operation returned.
    Result,
    /// The error the operation failed with.
    Error,
    /// The root hash after the operation.
    RootHash,
    /// The `OperationCost` of proving the query.
    ProofCost,
    /// The proof bytes, or the error proving failed with, and what the
    /// verifier made of the proof.
    Proof,
}

impl Part {
    const ALL: [Part; 6] = [
        Part::Cost,
        Part::Result,
        Part::Error,
        Part::RootHash,
        Part::ProofCost,
        Part::Proof,
    ];

    /// The letter that introduces the part in a step line.
    pub fn tag(self) -> char {
        match self {
            Part::Cost => 'c',
            Part::Result => 'v',
            Part::Error => 'e',
            Part::RootHash => 'r',
            Part::ProofCost => 'q',
            Part::Proof => 'p',
        }
    }

    /// The name of the part in a report.
    pub fn name(self) -> &'static str {
        match self {
            Part::Cost => "cost",
            Part::Result => "result",
            Part::Error => "error",
            Part::RootHash => "root hash",
            Part::ProofCost => "proof cost",
            Part::Proof => "proof",
        }
    }

    fn of_tag(tag: char) -> Option<Part> {
        Part::ALL.into_iter().find(|part| part.tag() == tag)
    }
}

impl Step {
    /// The canonical bytes of the step, which [`TraceDigest`] hashes.
    ///
    /// Integers are fixed-width little-endian and byte strings carry a `u32`
    /// length. Every optional part is introduced by a presence byte, so no
    /// two steps share an encoding.
    pub fn encode(&self, out: &mut Vec<u8>) {
        put_cost(out, &self.cost);
        match &self.outcome {
            Outcome::Returned(value) => {
                out.push(0);
                value.encode(out);
            }
            Outcome::Failed(error) => {
                out.push(1);
                put_bytes(out, error.as_bytes());
            }
        }
        match &self.root_hash {
            Some(root_hash) => {
                out.push(1);
                out.extend_from_slice(root_hash);
            }
            None => out.push(0),
        }
        match &self.proof {
            Some(proof) => {
                out.push(1);
                put_cost(out, &proof.cost);
                put_proved(out, &proof.proved);
            }
            None => out.push(0),
        }
    }

    /// The parts of the step, each with the bytes its digest is taken over.
    fn parts(&self) -> Vec<(Part, Vec<u8>)> {
        let mut parts = Vec::with_capacity(4);
        let mut cost = Vec::new();
        put_cost(&mut cost, &self.cost);
        parts.push((Part::Cost, cost));
        match &self.outcome {
            Outcome::Returned(Value::Nothing) => {}
            Outcome::Returned(value) => {
                let mut bytes = Vec::new();
                value.encode(&mut bytes);
                parts.push((Part::Result, bytes));
            }
            Outcome::Failed(error) => parts.push((Part::Error, error.clone().into_bytes())),
        }
        if let Some(root_hash) = &self.root_hash {
            parts.push((Part::RootHash, root_hash.to_vec()));
        }
        if let Some(proof) = &self.proof {
            let mut cost = Vec::new();
            put_cost(&mut cost, &proof.cost);
            parts.push((Part::ProofCost, cost));
            let mut proved = Vec::new();
            put_proved(&mut proved, &proof.proved);
            parts.push((Part::Proof, proved));
        }
        parts
    }

    /// The short form of the step: for each part its letter and the first
    /// 16 bits of the BLAKE3 digest of its bytes, as four hex digits.
    ///
    /// Sixteen bits are enough to point at the part that differs. Whether
    /// two traces are the same is decided by [`TraceDigest`], never by
    /// these.
    pub fn line(&self) -> String {
        let mut line = String::new();
        for (part, bytes) in self.parts() {
            let digest = blake3::hash(&bytes);
            line.push(part.tag());
            line.push_str(&hex::encode(&digest.as_bytes()[..2]));
        }
        line
    }

    /// The long form of the step, one line per value, without the leading
    /// indentation a trace file gives it.
    pub fn detail(&self) -> Vec<String> {
        let mut lines = vec![format!("cost {}", cost_text(&self.cost))];
        match &self.outcome {
            Outcome::Returned(value) => value.detail(&mut lines),
            Outcome::Failed(error) => lines.push(format!("failed {error}")),
        }
        if let Some(root_hash) = &self.root_hash {
            lines.push(format!("root {}", hex::encode(root_hash)));
        }
        if let Some(proof) = &self.proof {
            lines.push(format!("prove-cost {}", cost_text(&proof.cost)));
            match &proof.proved {
                Proved::Bytes(bytes, verification) => {
                    lines.push(format!("proof {} {}", bytes.len(), hex::encode(bytes)));
                    lines.push(match verification {
                        Verification::Verified => "verification verified".to_string(),
                        Verification::OtherRoot => "verification other-root".to_string(),
                        Verification::Refused(error) => format!("verification refused {error}"),
                    });
                }
                Proved::Failed(error) => lines.push(format!("prove-failed {error}")),
            }
        }
        lines
    }

    /// Whether the operation failed.
    pub fn failed(&self) -> bool {
        matches!(self.outcome, Outcome::Failed(_))
    }

    /// Whether the step is a query GroveDB could not prove.
    pub fn unproved(&self) -> bool {
        matches!(
            self.proof,
            Some(Proof {
                proved: Proved::Failed(_),
                ..
            })
        )
    }

    /// Whether the step is a query whose proof does not verify to the root
    /// hash of the committed state.
    pub fn unverified(&self) -> bool {
        matches!(
            self.proof,
            Some(Proof {
                proved: Proved::Bytes(_, Verification::OtherRoot | Verification::Refused(_)),
                ..
            })
        )
    }

    /// How many bytes of results and proofs the long form writes out.
    pub fn payload_len(&self) -> usize {
        let returned = match &self.outcome {
            Outcome::Returned(value) => value.payload_len(),
            Outcome::Failed(_) => 0,
        };
        let proved = match &self.proof {
            Some(Proof {
                proved: Proved::Bytes(bytes, _),
                ..
            }) => bytes.len(),
            _ => 0,
        };
        returned + proved
    }
}

/// The parts in which two step lines differ. A part only one of them has
/// differs. `None` when either is not a step line.
pub fn differing_parts(expected: &str, actual: &str) -> Option<Vec<Part>> {
    let expected = parse_line(expected)?;
    let actual = parse_line(actual)?;
    let mut parts: Vec<Part> = Part::ALL
        .into_iter()
        .filter(|part| {
            let digest = |fields: &[(Part, String)]| {
                fields
                    .iter()
                    .find(|(field, _)| field == part)
                    .map(|(_, digest)| digest.clone())
            };
            digest(&expected) != digest(&actual)
        })
        .collect();
    parts.sort();
    Some(parts)
}

/// Whether `line` has the form [`Step::line`] writes.
pub fn is_step_line(line: &str) -> bool {
    parse_line(line).is_some_and(|fields| !fields.is_empty())
}

fn parse_line(line: &str) -> Option<Vec<(Part, String)>> {
    let mut fields = Vec::new();
    let mut rest = line;
    while let Some(tag) = rest.chars().next() {
        let part = Part::of_tag(tag)?;
        let digest = rest.get(1..5)?;
        if !digest
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
        {
            return None;
        }
        if fields.iter().any(|(seen, _)| *seen == part) {
            return None;
        }
        fields.push((part, digest.to_string()));
        rest = &rest[5..];
    }
    Some(fields)
}

/// A cost as text: every field by name. Removed bytes are `-` when the
/// operation reported no removal, a number for a basic removal, and one
/// `owner/epoch=bytes` entry per section otherwise.
pub fn cost_text(cost: &OperationCost) -> String {
    let mut text = format!(
        "seeks={} loaded={} added={} replaced={} removed=",
        cost.seek_count,
        cost.storage_loaded_bytes,
        cost.storage_cost.added_bytes,
        cost.storage_cost.replaced_bytes,
    );
    match &cost.storage_cost.removed_bytes {
        StorageRemovedBytes::NoStorageRemoval => text.push('-'),
        StorageRemovedBytes::BasicStorageRemoval(bytes) => {
            let _ = write!(text, "{bytes}");
        }
        StorageRemovedBytes::SectionedStorageRemoval(sections) => {
            text.push('{');
            let mut first = true;
            for (owner, epochs) in sections {
                for (epoch, bytes) in sorted_epochs(epochs.iter()) {
                    if !first {
                        text.push(',');
                    }
                    first = false;
                    let _ = write!(text, "{}/{epoch}={bytes}", hex::encode(owner));
                }
            }
            text.push('}');
        }
    }
    let _ = write!(
        text,
        " hashes={} sinsemilla={}",
        cost.hash_node_calls, cost.sinsemilla_hash_calls
    );
    text
}

/// The sections of one owner by epoch: the map GroveDB keeps them in has no
/// order of its own.
fn sorted_epochs<'a>(epochs: impl Iterator<Item = (u16, &'a u32)>) -> Vec<(u16, u32)> {
    let mut epochs: Vec<(u16, u32)> = epochs.map(|(epoch, bytes)| (epoch, *bytes)).collect();
    epochs.sort_unstable();
    epochs
}

fn put_cost(out: &mut Vec<u8>, cost: &OperationCost) {
    out.extend_from_slice(&cost.seek_count.to_le_bytes());
    out.extend_from_slice(&cost.storage_loaded_bytes.to_le_bytes());
    out.extend_from_slice(&cost.storage_cost.added_bytes.to_le_bytes());
    out.extend_from_slice(&cost.storage_cost.replaced_bytes.to_le_bytes());
    match &cost.storage_cost.removed_bytes {
        StorageRemovedBytes::NoStorageRemoval => out.push(0),
        StorageRemovedBytes::BasicStorageRemoval(bytes) => {
            out.push(1);
            out.extend_from_slice(&bytes.to_le_bytes());
        }
        StorageRemovedBytes::SectionedStorageRemoval(sections) => {
            out.push(2);
            put_len(out, sections.len());
            for (owner, epochs) in sections {
                out.extend_from_slice(owner);
                let epochs = sorted_epochs(epochs.iter());
                put_len(out, epochs.len());
                for (epoch, bytes) in epochs {
                    out.extend_from_slice(&epoch.to_le_bytes());
                    out.extend_from_slice(&bytes.to_le_bytes());
                }
            }
        }
    }
    out.extend_from_slice(&cost.hash_node_calls.to_le_bytes());
    out.extend_from_slice(&cost.sinsemilla_hash_calls.to_le_bytes());
}

fn put_proved(out: &mut Vec<u8>, proved: &Proved) {
    match proved {
        Proved::Bytes(bytes, verification) => {
            out.push(0);
            put_bytes(out, bytes);
            match verification {
                Verification::Verified => out.push(0),
                Verification::OtherRoot => out.push(1),
                Verification::Refused(error) => {
                    out.push(2);
                    put_bytes(out, error.as_bytes());
                }
            }
        }
        Proved::Failed(error) => {
            out.push(1);
            put_bytes(out, error.as_bytes());
        }
    }
}

fn put_len(out: &mut Vec<u8>, len: usize) {
    let len = u32::try_from(len).expect("a recorded length fits in a u32");
    out.extend_from_slice(&len.to_le_bytes());
}

/// Appends `bytes` with their length, the framing every recorded byte string
/// uses.
pub(crate) fn put_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    put_len(out, bytes.len());
    out.extend_from_slice(bytes);
}

/// The running digest of a trace: every step, in order.
///
/// Two traces have the same digest exactly when every step of one equals
/// the same step of the other, in cost, result, root hash and proof.
#[derive(Clone, Debug, Default)]
pub struct TraceDigest {
    hasher: blake3::Hasher,
    steps: u64,
    scratch: Vec<u8>,
}

impl TraceDigest {
    /// An empty digest.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds one step.
    pub fn update(&mut self, step: &Step) {
        self.scratch.clear();
        step.encode(&mut self.scratch);
        let len = u32::try_from(self.scratch.len()).expect("an encoded step fits in a u32");
        self.hasher.update(&len.to_le_bytes());
        self.hasher.update(&self.scratch);
        self.steps += 1;
    }

    /// The number of steps added.
    pub fn steps(&self) -> u64 {
        self.steps
    }

    /// The digest of everything added so far, as lower-case hex.
    pub fn hex(&self) -> String {
        let mut hasher = self.hasher.clone();
        hasher.update(&self.steps.to_le_bytes());
        hex::encode(hasher.finalize().as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use grovedb_costs::storage_cost::StorageCost;

    use super::*;

    fn cost(seeks: u32, removed: StorageRemovedBytes) -> OperationCost {
        OperationCost {
            seek_count: seeks,
            storage_cost: StorageCost {
                added_bytes: 11,
                replaced_bytes: 22,
                removed_bytes: removed,
            },
            storage_loaded_bytes: 33,
            hash_node_calls: 44,
            sinsemilla_hash_calls: 55,
        }
    }

    fn sectioned() -> StorageRemovedBytes {
        let epochs = [(7u16, 70u32), (3u16, 30u32)].into_iter().collect();
        let sections = BTreeMap::from([([0xAB; 32], epochs)]);
        StorageRemovedBytes::SectionedStorageRemoval(sections)
    }

    fn write_step() -> Step {
        Step {
            cost: cost(1, StorageRemovedBytes::NoStorageRemoval),
            outcome: Outcome::Returned(Value::Nothing),
            root_hash: Some([9; 32]),
            proof: None,
        }
    }

    fn query_step() -> Step {
        Step {
            cost: cost(2, StorageRemovedBytes::BasicStorageRemoval(5)),
            outcome: Outcome::Returned(Value::Element(vec![1, 2, 3])),
            root_hash: None,
            proof: Some(Proof {
                cost: cost(3, StorageRemovedBytes::NoStorageRemoval),
                proved: Proved::Bytes(vec![4, 5], Verification::Verified),
            }),
        }
    }

    fn row(path: &[&[u8]], key: &[u8], element: &[u8]) -> Row {
        Row {
            path: path.iter().map(|segment| segment.to_vec()).collect(),
            key: key.to_vec(),
            element: element.to_vec(),
        }
    }

    fn encoded(step: &Step) -> Vec<u8> {
        let mut out = Vec::new();
        step.encode(&mut out);
        out
    }

    #[test]
    fn a_cost_is_written_field_by_field() {
        assert_eq!(
            cost_text(&cost(1, StorageRemovedBytes::NoStorageRemoval)),
            "seeks=1 loaded=33 added=11 replaced=22 removed=- hashes=44 sinsemilla=55"
        );
        assert_eq!(
            cost_text(&cost(1, StorageRemovedBytes::BasicStorageRemoval(0))),
            "seeks=1 loaded=33 added=11 replaced=22 removed=0 hashes=44 sinsemilla=55"
        );
        let owner = "ab".repeat(32);
        assert_eq!(
            cost_text(&cost(1, sectioned())),
            format!(
                "seeks=1 loaded=33 added=11 replaced=22 removed={{{owner}/3=30,{owner}/7=70}} \
                 hashes=44 sinsemilla=55"
            )
        );
    }

    #[test]
    fn every_field_of_a_cost_changes_the_encoding() {
        let base = cost(1, StorageRemovedBytes::NoStorageRemoval);
        let mut variants = vec![
            base.clone(),
            cost(2, StorageRemovedBytes::NoStorageRemoval),
            cost(1, StorageRemovedBytes::BasicStorageRemoval(0)),
            cost(1, StorageRemovedBytes::BasicStorageRemoval(1)),
            cost(1, sectioned()),
        ];
        let mut other = base.clone();
        other.storage_cost.added_bytes += 1;
        variants.push(other);
        let mut other = base.clone();
        other.storage_cost.replaced_bytes += 1;
        variants.push(other);
        let mut other = base.clone();
        other.storage_loaded_bytes += 1;
        variants.push(other);
        let mut other = base.clone();
        other.hash_node_calls += 1;
        variants.push(other);
        let mut other = base;
        other.sinsemilla_hash_calls += 1;
        variants.push(other);
        let encodings: std::collections::BTreeSet<Vec<u8>> = variants
            .iter()
            .map(|cost| {
                let mut out = Vec::new();
                put_cost(&mut out, cost);
                out
            })
            .collect();
        assert_eq!(encodings.len(), variants.len());
    }

    #[test]
    fn every_part_of_a_step_changes_its_encoding_and_its_line() {
        let base = query_step();
        let mut changed = Vec::new();
        let mut step = base.clone();
        step.cost.seek_count += 1;
        changed.push((step, vec![Part::Cost]));
        let mut step = base.clone();
        step.outcome = Outcome::Returned(Value::Element(vec![1, 2, 4]));
        changed.push((step, vec![Part::Result]));
        for rows in [
            Vec::new(),
            vec![row(&[], b"", &[1, 2, 3])],
            vec![row(&[b"a"], b"k", &[1, 2, 3])],
            vec![row(&[b"a", b""], b"k", &[1, 2, 3])],
            vec![row(&[b"a"], b"", &[b'k', 1, 2, 3])],
            vec![row(&[b"a"], b"k", &[1]), row(&[b"a"], b"l", &[2])],
            vec![row(&[b"a"], b"l", &[2]), row(&[b"a"], b"k", &[1])],
        ] {
            let mut step = base.clone();
            step.outcome = Outcome::Returned(Value::Rows(rows));
            changed.push((step, vec![Part::Result]));
        }
        let mut step = base.clone();
        step.outcome = Outcome::Returned(Value::Nothing);
        changed.push((step, vec![Part::Result]));
        let mut step = base.clone();
        step.outcome = Outcome::Failed("PathKeyNotFound".to_string());
        changed.push((step, vec![Part::Result, Part::Error]));
        let mut step = base.clone();
        step.root_hash = Some([1; 32]);
        changed.push((step, vec![Part::RootHash]));
        let mut step = base.clone();
        step.proof.as_mut().unwrap().cost.hash_node_calls += 1;
        changed.push((step, vec![Part::ProofCost]));
        for proved in [
            Proved::Bytes(vec![4, 6], Verification::Verified),
            Proved::Bytes(vec![4, 5], Verification::OtherRoot),
            Proved::Bytes(
                vec![4, 5],
                Verification::Refused("InvalidProof".to_string()),
            ),
            Proved::Failed("NotSupported".to_string()),
        ] {
            let mut step = base.clone();
            step.proof.as_mut().unwrap().proved = proved;
            changed.push((step, vec![Part::Proof]));
        }
        let mut step = base.clone();
        step.proof = None;
        changed.push((step, vec![Part::ProofCost, Part::Proof]));

        let mut encodings = std::collections::BTreeSet::new();
        encodings.insert(encoded(&base));
        for (step, parts) in &changed {
            assert!(
                encodings.insert(encoded(step)),
                "{step:?} shares an encoding"
            );
            assert_eq!(
                differing_parts(&base.line(), &step.line()).as_ref(),
                Some(parts),
                "{step:?}"
            );
        }
        assert_eq!(
            differing_parts(&base.line(), &base.line()),
            Some(Vec::new())
        );
    }

    #[test]
    fn the_short_and_long_forms_name_the_same_parts() {
        let write = write_step();
        assert_eq!(write.line().len(), 10);
        assert!(write.line().starts_with('c') && write.line()[5..].starts_with('r'));
        assert_eq!(
            write.detail(),
            [
                "cost seeks=1 loaded=33 added=11 replaced=22 removed=- hashes=44 sinsemilla=55",
                &format!("root {}", "09".repeat(32)),
            ]
        );
        assert_eq!(write.payload_len(), 0);

        let query = query_step();
        let tags: String = query.line().chars().step_by(5).collect();
        assert_eq!(tags, "cvqp");
        assert_eq!(
            query.detail(),
            [
                "cost seeks=2 loaded=33 added=11 replaced=22 removed=5 hashes=44 sinsemilla=55",
                "element 010203",
                "prove-cost seeks=3 loaded=33 added=11 replaced=22 removed=- hashes=44 \
                 sinsemilla=55",
                "proof 2 0405",
                "verification verified",
            ]
        );
        assert_eq!(query.payload_len(), 5);

        let mut refused = query.clone();
        refused.proof.as_mut().unwrap().proved =
            Proved::Bytes(vec![7], Verification::Refused("InvalidProof".to_string()));
        assert_eq!(
            refused.detail().last().unwrap(),
            "verification refused InvalidProof"
        );
        let mut other_root = query.clone();
        other_root.proof.as_mut().unwrap().proved = Proved::Bytes(vec![7], Verification::OtherRoot);
        assert_eq!(
            other_root.detail().last().unwrap(),
            "verification other-root"
        );
        assert_eq!(
            (query.failed(), query.unproved(), query.unverified()),
            (false, false, false)
        );
        let mut unproved = query;
        unproved.outcome = Outcome::Failed("PathKeyNotFound".to_string());
        unproved.proof.as_mut().unwrap().proved = Proved::Failed("NotSupported".to_string());
        assert_eq!(unproved.detail()[1], "failed PathKeyNotFound");
        assert_eq!(
            unproved.detail().last().unwrap(),
            "prove-failed NotSupported"
        );
        assert_eq!(unproved.payload_len(), 0);
        let tags: String = unproved.line().chars().step_by(5).collect();
        assert_eq!(tags, "ceqp");

        let flags = |step: &Step| (step.failed(), step.unproved(), step.unverified());
        assert_eq!(flags(&write), (false, false, false));
        assert_eq!(flags(&refused), (false, false, true));
        assert_eq!(flags(&other_root), (false, false, true));
        assert_eq!(flags(&unproved), (true, true, false));
    }

    #[test]
    fn rows_are_written_one_per_line() {
        let mut step = write_step();
        step.outcome = Outcome::Returned(Value::Rows(vec![
            row(&[], b"", &[1]),
            row(&[b"\x0a", b"", b"\x0b\x0c"], b"\xff", &[2, 3]),
        ]));
        assert_eq!(
            step.detail()[1..4],
            ["rows 2", "row - - 01", "row 0a/-/0b0c ff 0203"]
        );
        assert_eq!(step.payload_len(), 1 + 3 + 1 + 2);
        step.outcome = Outcome::Returned(Value::Rows(Vec::new()));
        assert_eq!(step.detail()[1], "rows 0");
        let tags: String = step.line().chars().step_by(5).collect();
        assert_eq!(tags, "cvr");
    }

    #[test]
    fn only_step_lines_are_step_lines() {
        assert!(is_step_line(&write_step().line()));
        assert!(is_step_line(&query_step().line()));
        for line in [
            "",
            "c12",
            "c12g4",
            "x1234",
            "c1234c1234",
            "c12345",
            "C1234",
            "cABCD",
        ] {
            assert!(!is_step_line(line), "{line:?}");
        }
        assert_eq!(differing_parts("c0000", "nonsense"), None);
        assert_eq!(differing_parts("nonsense", "c0000"), None);
        assert_eq!(
            differing_parts("c0000r1111", "c0000"),
            Some(vec![Part::RootHash])
        );
        for part in Part::ALL {
            assert_eq!(Part::of_tag(part.tag()), Some(part));
            assert!(!part.name().is_empty());
        }
    }

    #[test]
    fn the_trace_digest_follows_every_step_and_their_order() {
        let (write, query) = (write_step(), query_step());
        let digest = |steps: &[&Step]| {
            let mut digest = TraceDigest::new();
            for step in steps {
                digest.update(step);
            }
            (digest.steps(), digest.hex())
        };
        let empty = digest(&[]);
        assert_eq!(empty.0, 0);
        assert_eq!(empty.1.len(), 64);
        let both = digest(&[&write, &query]);
        assert_eq!(both.0, 2);
        assert_eq!(both, digest(&[&write, &query]));
        assert_ne!(both.1, digest(&[&query, &write]).1);
        assert_ne!(both.1, digest(&[&write]).1);
        assert_ne!(digest(&[&write]).1, empty.1);
        let mut changed = query.clone();
        changed.proof.as_mut().unwrap().proved = Proved::Bytes(vec![4, 5], Verification::OtherRoot);
        assert_ne!(both.1, digest(&[&write, &changed]).1);
    }
}
