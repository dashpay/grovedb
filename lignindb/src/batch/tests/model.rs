//! Random schedules of calls, against GM0 written as a closed form: the
//! outcome of a key is decided by the latest generation that has a call on
//! it, as the last put of that generation, or with none its first delete.

use std::collections::BTreeMap;

use super::{leaf, subtree, Rng};
use crate::{
    batch::{StagedBatch, Written},
    txn::{Column, Hosted, Unit},
};

/// A key of the model: the place of its column in the order of a batch,
/// the name of its subtree, and the key.
type Key = (usize, u8, u8);

const COLUMNS: [Column; 4] = [Column::Meta, Column::Aux, Column::Roots, Column::Data];

struct Call {
    key: Key,
    /// The value of a put, or `None` for a delete.
    put: Option<u8>,
    generation: u64,
    tag: u32,
}

#[derive(Default)]
struct Model {
    generation: u64,
    calls: Vec<Call>,
}

impl Model {
    fn call(&mut self, key: Key, put: Option<u8>, tag: u32) {
        self.calls.push(Call {
            key,
            put,
            generation: self.generation,
            tag,
        });
    }

    fn outcomes(&self) -> BTreeMap<Key, (Option<u8>, u32)> {
        let mut latest: BTreeMap<Key, u64> = BTreeMap::new();
        for call in &self.calls {
            let at = latest.entry(call.key).or_default();
            *at = (*at).max(call.generation);
        }
        (latest.into_iter())
            .map(|(key, generation)| {
                let calls = || {
                    (self.calls.iter())
                        .filter(move |call| call.key == key && call.generation == generation)
                };
                let last_put = calls().rfind(|call| call.put.is_some());
                let stands = last_put.or_else(|| calls().next()).expect("a call");
                (key, (stands.put, stands.tag))
            })
            .collect()
    }

    fn merge(&mut self, other: Self) {
        for (key, (put, tag)) in other.outcomes() {
            self.call(key, put, tag);
        }
    }

    fn merge_overwriting(&mut self, other: Self) {
        self.generation += 1;
        self.merge(other);
        self.generation += 1;
    }
}

/// One batch with its model. With `nodes` the data column holds nodes.
struct Pair {
    batch: StagedBatch<u32>,
    model: Model,
    nodes: bool,
}

impl Pair {
    fn new(nodes: bool) -> Self {
        Self {
            batch: StagedBatch::new(),
            model: Model::default(),
            nodes,
        }
    }

    fn call(&mut self, key: Key, put: Option<u8>, tag: u32) {
        let (column, name, at) = (COLUMNS[key.0], key.1, [key.2]);
        let unit = Unit::new(column, subtree(name));
        match (put, self.nodes && column == Column::Data) {
            (Some(value), false) => self.batch.put(unit, &at, &[value], tag),
            (None, false) => self.batch.delete(unit, &at, tag),
            (Some(value), true) => {
                let node = leaf(&at, &[value]);
                self.batch
                    .put_node(unit.subtree, &node.put(true, Hosted::Keep), tag);
            }
            (None, true) => self.batch.delete_node(unit.subtree, &at, tag),
        }
        self.model.call(key, put, tag);
    }

    fn next_operation(&mut self) {
        self.batch.next_operation();
        self.model.generation += 1;
    }

    fn take(&mut self) -> Self {
        let taken = Self {
            batch: self.batch.take_pending(),
            model: std::mem::take(&mut self.model),
            nodes: self.nodes,
        };
        assert!(
            self.batch.is_empty(),
            "what is taken leaves the batch empty"
        );
        taken
    }

    fn check(self, seed: u64) {
        let expected = self.model.outcomes();
        assert_eq!(self.batch.len(), expected.len(), "seed {seed}");
        let resolved = self.batch.resolve().expect("no call is refused");
        let outcomes: Vec<(Key, (Option<u8>, u32))> = (resolved.outcomes())
            .map(|outcome| {
                let column = COLUMNS
                    .iter()
                    .position(|column| *column == outcome.unit.column);
                let value = match outcome.write {
                    Written::Record(value) => Some(value[0]),
                    Written::Node(node) => Some(node.value.as_deref().expect("a stated value")[0]),
                    Written::Removed | Written::NodeRemoved => None,
                };
                let is_node = matches!(outcome.write, Written::Node(_) | Written::NodeRemoved);
                assert_eq!(is_node, self.nodes && outcome.unit.column == Column::Data);
                let name = outcome.unit.subtree.as_bytes()[0];
                (
                    (column.expect("a column"), name, outcome.key[0]),
                    (value, *outcome.tag),
                )
            })
            .collect();
        // The order of the outcomes is the order of the keys.
        assert_eq!(
            outcomes,
            expected.into_iter().collect::<Vec<_>>(),
            "seed {seed}"
        );
    }
}

#[test]
fn every_schedule_of_calls_leaves_the_outcome_gm0_names() {
    for seed in 1..=1500u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let nodes = seed % 2 == 0;
        let (mut batch, mut other) = (Pair::new(nodes), Pair::new(nodes));
        let mut tag = 0;
        for _ in 0..10 + rng.below(50) {
            tag += 1;
            let into = match rng.below(3) {
                0 => &mut other,
                _ => &mut batch,
            };
            let key = (rng.below(4), 1 + rng.below(2) as u8, rng.below(3) as u8);
            match rng.below(20) {
                0..=7 => into.call(key, Some(rng.below(250) as u8), tag),
                8..=14 => into.call(key, None, tag),
                15 | 16 => into.next_operation(),
                17 => {
                    let merged = std::mem::replace(&mut other, Pair::new(nodes));
                    batch.batch.merge(merged.batch);
                    batch.model.merge(merged.model);
                }
                18 => {
                    let merged = std::mem::replace(&mut other, Pair::new(nodes));
                    batch.batch.merge_overwriting(merged.batch);
                    batch.model.merge_overwriting(merged.model);
                }
                _ => {
                    // What is taken comes back as a later operation.
                    let taken = batch.take();
                    batch.batch.merge_overwriting(taken.batch);
                    batch.model.merge_overwriting(taken.model);
                }
            }
        }
        batch.check(seed);
        other.check(seed);
    }
}
