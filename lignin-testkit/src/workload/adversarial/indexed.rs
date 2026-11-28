//! Indexed trees written through their own methods.

use std::collections::BTreeMap;

use grovedb::{Element, PathQuery, Query};
use grovedb_merk::proofs::query::IndexAxis;

use super::DOMAIN_INDEXED;
use crate::workload::{
    build::{self, batch, child, path, sum, value},
    op::{IndexedTree, Op, Write},
    rng::Rng,
    zoo::TreeKind,
    Features, Prepared, Stream,
};

const ROOT: &[u8] = b"indexed";

/// The most rounds [`IndexedWrites`] runs: a key holds its round in one
/// byte.
pub const MAX_INDEXED_ROUNDS: u32 = 256;

/// The three types of indexed tree, written entry by entry.
///
/// An indexed tree keeps, beside its entries, a secondary tree for each
/// aggregate it ranks them by, and every write to an entry is mirrored
/// there. The other workloads write indexed trees through batches. This one
/// uses the methods GroveDB has for each type: an entry is inserted, written
/// again with another aggregate, which moves it in the ranking, and
/// deleted, and deleting a key that is not there does nothing. An entry of
/// the tree that ranks by count may be a tree itself, which is filled
/// through ordinary inserts and deleted with what it holds. Each round ends
/// with `reconcile_indexed_tree_secondaries`, which rebuilds the secondary
/// trees from the entries and has nothing to repair in a tree every grove
/// version from the fifth keeps in step.
///
/// After each block every tree is listed as stored and, where the grove
/// version reads by rank, its top entries are read and proven.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndexedWrites {
    /// How many rounds to run, at most [`MAX_INDEXED_ROUNDS`].
    pub rounds: u32,
    /// What the target grove version can store.
    pub features: Features,
}

impl Default for IndexedWrites {
    fn default() -> Self {
        Self {
            rounds: 5,
            features: Features::ALL,
        }
    }
}

/// One indexed tree and the keys stored in it.
struct Indexed {
    tree: IndexedTree,
    key: &'static [u8],
    element: Element,
    axes: &'static [IndexAxis],
    /// The entries, each with whether it is a tree.
    entries: BTreeMap<Vec<u8>, bool>,
}

impl Indexed {
    /// An entry of the kind the tree ranks.
    fn entry(&self, rng: &mut Rng) -> Element {
        match self.tree {
            IndexedTree::Count => Element::new_item(value(rng, 1, 40)),
            IndexedTree::Sum | IndexedTree::BigSum => Element::new_sum_item(sum(rng)),
            IndexedTree::CountSum | IndexedTree::CountBigSum => {
                Element::new_item_with_sum_item(value(rng, 1, 40), sum(rng))
            }
        }
    }
}

impl IndexedWrites {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(
            self.rounds <= MAX_INDEXED_ROUNDS,
            "a key holds its round in one byte"
        );
        let mut rng = Rng::derive(seed, DOMAIN_INDEXED, 0);
        let root = path(&[ROOT]);
        let mut trees = [
            (
                IndexedTree::Count,
                &b"by-count"[..],
                TreeKind::CountIndexed,
                &[IndexAxis::Count][..],
            ),
            (
                IndexedTree::Sum,
                b"by-sum",
                TreeKind::SumIndexed,
                &[IndexAxis::Sum],
            ),
            (
                IndexedTree::CountSum,
                b"by-both",
                TreeKind::CountSumIndexed,
                &[IndexAxis::Count, IndexAxis::Sum, IndexAxis::Avg],
            ),
        ]
        .map(|(tree, key, kind, axes)| Indexed {
            tree,
            key,
            element: kind.element(),
            axes,
            entries: BTreeMap::new(),
        });
        let mut ops = vec![build::insert(&[], ROOT, Element::empty_tree())];
        for indexed in &trees {
            ops.push(build::insert(&root, indexed.key, indexed.element.clone()));
        }
        ops.push(Op::Commit);

        for round in 0..self.rounds {
            for indexed in &mut trees {
                self.round(round, &root, indexed, &mut rng, &mut ops);
            }
            ops.push(Op::Commit);
            for indexed in &trees {
                self.reads(round, &root, indexed, &mut rng, &mut ops);
            }
        }
        Prepared::stream(ops)
    }

    fn round(
        &self,
        round: u32,
        root: &[Vec<u8>],
        indexed: &mut Indexed,
        rng: &mut Rng,
        ops: &mut Vec<Op>,
    ) {
        let tree = child(root, indexed.key);
        let insert = |key: &[u8], element: Element| {
            Op::Write(Write::IndexedInsert {
                path: tree.clone(),
                key: key.to_vec(),
                element,
                tree: indexed.tree,
            })
        };
        let delete = |key: &[u8]| {
            Op::Write(Write::IndexedDelete {
                path: tree.clone(),
                key: key.to_vec(),
                tree: indexed.tree,
            })
        };

        for entry in 0..4u8 {
            let key = vec![b'e', round as u8, entry];
            ops.push(insert(&key, indexed.entry(rng)));
            indexed.entries.insert(key, false);
        }
        // A tree as an entry: the tree that ranks by count ranks it by how
        // much it holds.
        if indexed.tree == IndexedTree::Count {
            let key = vec![b'g', round as u8];
            ops.push(insert(&key, Element::empty_count_tree()));
            for row in 0..=round as u8 % 3 {
                ops.push(build::insert(
                    &child(&tree, &key),
                    &[row],
                    Element::new_item(value(rng, 1, 20)),
                ));
            }
            indexed.entries.insert(key, true);
        }
        // One entry through a batch, as the other workloads write them.
        let batched = vec![b'b', round as u8];
        ops.push(Op::Batch(vec![batch::insert_or_replace(
            &tree,
            &batched,
            indexed.entry(rng),
        )]));
        indexed.entries.insert(batched, false);

        // An entry written again changes its place in the ranking.
        let values: Vec<Vec<u8>> = indexed
            .entries
            .iter()
            .filter(|(_, is_tree)| !**is_tree)
            .map(|(key, _)| key.clone())
            .collect();
        for _ in 0..2 {
            let key = &values[rng.index(values.len())];
            ops.push(insert(key, indexed.entry(rng)));
        }

        // Deletes: an entry, a tree with what it holds, and a key that is
        // not stored.
        let key = values[rng.index(values.len())].clone();
        ops.push(delete(&key));
        indexed.entries.remove(&key);
        if round % 2 == 1 {
            let group = indexed
                .entries
                .iter()
                .find(|(_, is_tree)| **is_tree)
                .map(|(key, _)| key.clone());
            if let Some(group) = group {
                ops.push(delete(&group));
                indexed.entries.remove(&group);
            }
        }
        ops.push(delete(&[b'n', round as u8]));
        ops.push(Op::Write(Write::ReconcileIndexed { path: tree }));
    }

    fn reads(
        &self,
        round: u32,
        root: &[Vec<u8>],
        indexed: &Indexed,
        rng: &mut Rng,
        ops: &mut Vec<Op>,
    ) {
        let tree = child(root, indexed.key);
        ops.push(build::rows(
            PathQuery::new_unsized(tree.clone(), Query::new_range_full()),
            true,
        ));
        if self.features.read_modes {
            let axis = indexed.axes[round as usize % indexed.axes.len()];
            ops.push(build::proven(PathQuery::new_axis_top_k(
                tree,
                axis,
                rng.between(1, 4) as u16,
                rng.below(2),
                round.is_multiple_of(2),
            )));
        }
    }
}
