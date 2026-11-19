//! A model of the records of a database and of what a write transaction
//! reads, in maps: the committed records, the writes of each open
//! transaction with their first-write versions, and the conflict rule
//! evaluated directly (`transactions.md` RD3, RD5, CF2).
//!
//! The Merks of the model are treaps whose priorities come from the keys,
//! so the shape of a Merk follows from its keys alone, and a change writes
//! the nodes whose records differ, as a Merk does.

#![allow(dead_code)]

use std::collections::{BTreeMap, BTreeSet};

use lignin_format::tree_page::{Hash, Side};

use super::store::Rng;
use crate::txn::{
    Batch, Column, Hosted, LinkPut, MerkWrite, NodePut, NodeValue, NodeView, RecordWrite, RootPut,
    Subtree, Unit, ValuePut,
};

pub fn subtree(name: u8) -> Subtree {
    Subtree::new([name; 32])
}

fn mix(seed: u64, bytes: &[u8]) -> u64 {
    let mut state = seed ^ 0xcbf2_9ce4_8422_2325;
    for byte in bytes {
        state = (state ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
    }
    state ^ (state >> 29)
}

fn hash_of(parts: &[&[u8]]) -> Hash {
    let mut hash = [0; 32];
    let mut state = 0x1234_5678;
    for part in parts {
        state = mix(state, part).wrapping_add(part.len() as u64);
    }
    for (index, chunk) in hash.chunks_mut(8).enumerate() {
        state = mix(state.wrapping_add(index as u64), &state.to_le_bytes());
        chunk.copy_from_slice(&state.to_le_bytes());
    }
    hash
}

/// A link of a node record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRec {
    pub key: Vec<u8>,
    pub hash: Hash,
    pub heights: (u8, u8),
    pub aggregate: Vec<u8>,
}

/// The record of one Merk node, as Merk hands it to storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRec {
    pub value: Vec<u8>,
    pub feature: Vec<u8>,
    pub value_hash: Hash,
    pub kv_hash: Hash,
    pub left: Option<LinkRec>,
    pub right: Option<LinkRec>,
}

impl NodeRec {
    pub fn link(&self, side: Side) -> Option<&LinkRec> {
        match side {
            Side::Left => self.left.as_ref(),
            Side::Right => self.right.as_ref(),
        }
    }

    /// Checks that `view` shows this record.
    pub fn check(&self, key: &[u8], view: &NodeView<'_>) {
        assert_eq!(view.key(), key);
        assert_eq!(
            view.value(),
            NodeValue::Bytes(&self.value),
            "the value of {key:?}"
        );
        assert_eq!(view.value_len() as usize, self.value.len());
        assert_eq!(view.feature(), self.feature, "the feature of {key:?}");
        assert_eq!(view.value_hash(), &self.value_hash);
        assert_eq!(view.kv_hash(), &self.kv_hash);
        for side in [Side::Left, Side::Right] {
            let (shown, expected) = (view.link(side), self.link(side));
            assert_eq!(shown.is_some(), expected.is_some(), "a link of {key:?}");
            let (Some(shown), Some(expected)) = (shown, expected) else {
                continue;
            };
            assert_eq!(shown.hash, &expected.hash, "a child hash of {key:?}");
            assert_eq!(shown.heights, expected.heights);
            assert_eq!(shown.child_key_len as usize, expected.key.len());
            assert_eq!(shown.aggregate, expected.aggregate);
            if let Some(child) = shown.key {
                assert_eq!(child, expected.key);
            }
        }
    }
}

/// The node records of the Merk that holds `items`, by key, and the key of
/// its root.
pub fn records(
    items: &BTreeMap<Vec<u8>, Vec<u8>>,
) -> (Option<Vec<u8>>, BTreeMap<Vec<u8>, NodeRec>) {
    fn build(
        keys: &[(&Vec<u8>, &Vec<u8>)],
        out: &mut BTreeMap<Vec<u8>, NodeRec>,
    ) -> Option<(LinkRec, u8)> {
        let top = (0..keys.len()).max_by_key(|at| mix(7, keys[*at].0))?;
        let (key, value) = keys[top];
        let left = build(&keys[..top], out);
        let right = build(&keys[top + 1..], out);
        let count = keys.len() as u64;
        let value_hash = hash_of(&[value]);
        let kv_hash = hash_of(&[key, &value_hash]);
        let none = [0u8; 32];
        let hash = hash_of(&[
            &kv_hash,
            left.as_ref().map_or(&none, |(link, _)| &link.hash),
            right.as_ref().map_or(&none, |(link, _)| &link.hash),
        ]);
        let heights = (
            left.as_ref().map_or(0, |(_, height)| *height),
            right.as_ref().map_or(0, |(_, height)| *height),
        );
        let mut feature = vec![3];
        feature.extend_from_slice(&count.to_le_bytes());
        out.insert(
            key.clone(),
            NodeRec {
                value: value.clone(),
                feature: feature.clone(),
                value_hash,
                kv_hash,
                left: left.map(|(link, _)| link),
                right: right.map(|(link, _)| link),
            },
        );
        let link = LinkRec {
            key: key.clone(),
            hash,
            heights,
            aggregate: feature,
        };
        Some((link, heights.0.max(heights.1) + 1))
    }
    let keys: Vec<_> = items.iter().collect();
    let mut out = BTreeMap::new();
    let root = build(&keys, &mut out).map(|(link, _)| link.key);
    (root, out)
}

/// What a record key holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Val {
    Flat(Vec<u8>),
    Node(NodeRec),
}

/// One write of a transaction (ST5).
#[derive(Debug, Clone)]
pub enum Write {
    Put(Unit, Vec<u8>, Val),
    Delete(Unit, Vec<u8>),
    Range(Unit, Vec<u8>, Option<Vec<u8>>),
}

impl Write {
    pub fn unit(&self) -> Unit {
        match self {
            Self::Put(unit, ..) | Self::Delete(unit, _) | Self::Range(unit, ..) => *unit,
        }
    }

    fn covers(&self, unit: Unit, key: &[u8]) -> bool {
        match self {
            Self::Put(own, at, _) | Self::Delete(own, at) => *own == unit && at == key,
            Self::Range(own, start, end) => {
                *own == unit && key >= &start[..] && end.as_ref().is_none_or(|end| key < &end[..])
            }
        }
    }

    fn meets(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Range(unit, start, end), Self::Range(other, from, to)) => {
                let below = |low: &Vec<u8>, high: &Option<Vec<u8>>| {
                    high.as_ref().is_none_or(|high| low < high)
                };
                unit == other
                    && below(start, end)
                    && below(from, to)
                    && below(start, to)
                    && below(from, end)
            }
            (range @ Self::Range(..), Self::Put(unit, key, _) | Self::Delete(unit, key))
            | (Self::Put(unit, key, _) | Self::Delete(unit, key), range) => {
                range.covers(*unit, key)
            }
        }
    }
}

/// The records of a database by record key.
pub type Records = BTreeMap<(Unit, Vec<u8>), Val>;

fn apply(records: &mut Records, write: &Write) {
    match write {
        Write::Put(unit, key, value) => {
            records.insert((*unit, key.clone()), value.clone());
        }
        Write::Delete(unit, key) => {
            records.remove(&(*unit, key.clone()));
        }
        Write::Range(..) => records.retain(|(unit, key), _| !write.covers(*unit, key)),
    }
}

/// The model of one write transaction: its writes in order, each with its
/// first-write version and whether a later commit met it.
#[derive(Debug, Clone, Default)]
pub struct TxnModel {
    pub writes: Vec<(Write, u64, bool)>,
}

impl TxnModel {
    /// What the transaction's newest write that covers the key says:
    /// `None` if it has none, `Some(None)` for a delete.
    pub fn own(&self, unit: Unit, key: &[u8]) -> Option<Option<&Val>> {
        let newest = self
            .writes
            .iter()
            .rev()
            .find(|(write, ..)| write.covers(unit, key))?;
        Some(match &newest.0 {
            Write::Put(_, _, value) => Some(value),
            Write::Delete(..) | Write::Range(..) => None,
        })
    }

    /// The record the transaction reads under the key (RD3).
    pub fn read<'a>(&'a self, committed: &'a Records, unit: Unit, key: &[u8]) -> Option<&'a Val> {
        match self.own(unit, key) {
            Some(own) => own,
            None => committed.get(&(unit, key.to_vec())),
        }
    }

    /// The records of `unit` the transaction reads, in key order.
    pub fn view(&self, committed: &Records, unit: Unit) -> BTreeMap<Vec<u8>, Val> {
        let mut records: Records = committed
            .iter()
            .filter(|((own, _), _)| *own == unit)
            .map(|(key, value)| (key.clone(), value.clone()))
            .collect();
        for (write, ..) in &self.writes {
            if write.unit() == unit {
                apply(&mut records, write);
            }
        }
        records
            .into_iter()
            .map(|((_, key), value)| (key, value))
            .collect()
    }

    /// Whether the conflict rule fails the transaction's commit (CF2).
    pub fn conflicts(&self) -> bool {
        self.writes.iter().any(|(.., met)| *met)
    }

    /// How many writes are in the overlap set.
    pub fn met(&self) -> usize {
        self.writes.iter().filter(|(.., met)| *met).count()
    }

    /// A commit of `version` outside the transaction wrote `writes`.
    pub fn foreign(&mut self, version: u64, writes: &[Write]) {
        for (own, first_write, met) in &mut self.writes {
            if *first_write < version && writes.iter().any(|write| write.meets(own)) {
                *met = true;
            }
        }
    }
}

/// The committed records of the model.
#[derive(Debug, Clone, Default)]
pub struct Committed {
    pub records: Records,
    pub version: u64,
}

impl Committed {
    /// Commits `writes` as the next version.
    pub fn commit(&mut self, writes: &[Write]) -> u64 {
        for write in writes {
            apply(&mut self.records, write);
        }
        self.version += 1;
        self.version
    }

    /// The items of the Merk `subtree` as a reader of `records` has them.
    pub fn items(records: &BTreeMap<Vec<u8>, Val>) -> BTreeMap<Vec<u8>, Vec<u8>> {
        records
            .iter()
            .filter_map(|(key, value)| match value {
                Val::Node(node) => Some((key.clone(), node.value.clone())),
                Val::Flat(_) => None,
            })
            .collect()
    }
}

/// One operation, owned: what [`OwnedBatch::with`] makes a [`Batch`] of.
#[derive(Debug, Clone, Default)]
pub struct OwnedBatch {
    pub merks: Vec<OwnedMerk>,
    pub records: Vec<Write>,
}

#[derive(Debug, Clone)]
pub struct OwnedMerk {
    pub subtree: Subtree,
    /// The written nodes, each with whether its value is stated or kept.
    pub nodes: Vec<(Vec<u8>, NodeRec, bool)>,
    pub removed: Vec<Vec<u8>>,
    pub root: Option<Option<Vec<u8>>>,
    /// The Merks the nodes host, by key.
    pub hosted: BTreeMap<Vec<u8>, Vec<Subtree>>,
    /// Above which length a stated value goes out of line.
    pub out_of_line_above: usize,
    /// Whether the Merk has a host that the batch does not write.
    pub anchored: bool,
}

impl OwnedMerk {
    /// The change set that takes the Merk `subtree` from the records `view`
    /// to the Merk of `items`: the nodes whose records differ, and the keys
    /// that are gone.
    pub fn change(
        subtree: Subtree,
        view: &BTreeMap<Vec<u8>, Val>,
        items: &BTreeMap<Vec<u8>, Vec<u8>>,
    ) -> Self {
        let (root, new) = records(items);
        let old_root = records(&Committed::items(view)).0;
        let nodes = new
            .into_iter()
            .filter(|(key, node)| view.get(key) != Some(&Val::Node(node.clone())))
            .map(|(key, node)| {
                let keeps =
                    matches!(view.get(&key), Some(Val::Node(old)) if old.value == node.value);
                (key, node, !keeps)
            })
            .collect();
        let removed = view
            .keys()
            .filter(|key| !items.contains_key(*key))
            .cloned()
            .collect();
        Self {
            subtree,
            nodes,
            removed,
            root: (root != old_root).then_some(root),
            hosted: BTreeMap::new(),
            out_of_line_above: usize::MAX,
            anchored: false,
        }
    }

    /// The writes of the change set, for the model.
    pub fn writes(&self) -> Vec<Write> {
        let unit = Unit::data(self.subtree);
        let puts = (self.nodes.iter())
            .map(|(key, node, _)| Write::Put(unit, key.clone(), Val::Node(node.clone())));
        let deletes = (self.removed.iter()).map(|key| Write::Delete(unit, key.clone()));
        puts.chain(deletes).collect()
    }
}

impl OwnedBatch {
    /// Every write of the batch, in the order the write set takes them.
    pub fn writes(&self) -> Vec<Write> {
        let merks = self.merks.iter().flat_map(OwnedMerk::writes);
        merks.chain(self.records.iter().cloned()).collect()
    }

    /// Calls `run` with the batch.
    pub fn with<R>(&self, run: impl FnOnce(&Batch<'_>) -> R) -> R {
        fn link(link: &Option<LinkRec>) -> Option<LinkPut<'_>> {
            link.as_ref().map(|link| LinkPut {
                key: &link.key,
                hash: &link.hash,
                heights: link.heights,
                aggregate: &link.aggregate,
            })
        }
        fn put<'a>(merk: &'a OwnedMerk, node: &'a (Vec<u8>, NodeRec, bool)) -> NodePut<'a> {
            let (key, node, stated) = node;
            NodePut {
                key,
                feature: &node.feature,
                value_hash: &node.value_hash,
                kv_hash: &node.kv_hash,
                left: link(&node.left),
                right: link(&node.right),
                hosted: match merk.hosted.get(key) {
                    Some(hosted) => Hosted::Set(hosted),
                    None => Hosted::Keep,
                },
                value: match stated {
                    false => ValuePut::Keep,
                    true if node.value.len() > merk.out_of_line_above => {
                        ValuePut::OutOfLine(&node.value)
                    }
                    true => ValuePut::Inline(&node.value),
                },
                at: None,
            }
        }
        let nodes: Vec<Vec<NodePut<'_>>> = (self.merks.iter())
            .map(|merk| merk.nodes.iter().map(|node| put(merk, node)).collect())
            .collect();
        let removed: Vec<Vec<&[u8]>> = (self.merks.iter())
            .map(|merk| merk.removed.iter().map(|key| &key[..]).collect())
            .collect();
        let merks: Vec<MerkWrite<'_>> = (self.merks.iter().zip(&nodes).zip(&removed))
            .map(|((merk, nodes), removed)| MerkWrite {
                subtree: merk.subtree,
                nodes,
                removed,
                root: match &merk.root {
                    None => RootPut::Keep,
                    Some(root) => RootPut::Set(root.as_deref()),
                },
                anchored: merk.anchored,
            })
            .collect();
        let records: Vec<RecordWrite<'_>> = (self.records.iter())
            .map(|write| match write {
                Write::Put(unit, key, Val::Flat(value)) => RecordWrite::Put {
                    unit: *unit,
                    key,
                    value,
                },
                Write::Put(..) => panic!("a node is written in a change set"),
                Write::Delete(unit, key) => RecordWrite::Delete { unit: *unit, key },
                Write::Range(unit, start, end) => RecordWrite::DeleteRange {
                    unit: *unit,
                    start,
                    end: end.as_deref(),
                },
            })
            .collect();
        run(&Batch {
            records: &records,
            merks: &merks,
        })
    }
}

/// A value of `len` bytes drawn from `rng`.
pub fn value(rng: &mut Rng, len: usize) -> Vec<u8> {
    (0..len).map(|_| rng.next() as u8).collect()
}

/// The flat unit of `column` under the subtree `name`.
pub fn unit(column: Column, name: u8) -> Unit {
    Unit::new(column, subtree(name))
}

/// The keys of every record of the model that a test may ask for.
pub fn keys_of(records: &Records, unit: Unit) -> BTreeSet<Vec<u8>> {
    records
        .keys()
        .filter(|(own, _)| *own == unit)
        .map(|(_, key)| key.clone())
        .collect()
}
