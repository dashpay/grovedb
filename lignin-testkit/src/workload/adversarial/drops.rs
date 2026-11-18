//! Whole trees dropped without being opened.

use grovedb::Element;

use super::{
    stock::{stock, Stocked},
    DOMAIN_DROPS,
};
use crate::workload::{
    build::{self, batch, child, path, value},
    op::{Op, QueryItem},
    rng::Rng,
    Features, Prepared, Stream,
};

const ROOT: &[u8] = b"drops";

/// Flat drops of every type of tree.
///
/// Each round stores one populated tree of every type under one parent and
/// drops them all flat: some by a call of their own, some in a batch, alone,
/// in pairs or next to other writes. A flat drop removes the tree from its
/// parent, opens nothing below it and leaves a record of the storage to
/// reclaim; an indexed tree leaves its secondary trees in the same record.
/// The block is committed, the records are flushed, and the round ends by
/// storing new trees at some of the dropped keys, which must then hold only
/// what was stored last.
///
/// A stream for a grove version without flat drops deletes the same trees
/// with their contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FlatDrops {
    /// How many rounds to run.
    pub rounds: u32,
    /// What the target grove version can store.
    pub features: Features,
}

impl Default for FlatDrops {
    fn default() -> Self {
        Self {
            rounds: 3,
            features: Features::ALL,
        }
    }
}

impl FlatDrops {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        let mut rng = Rng::derive(seed, DOMAIN_DROPS, 0);
        let root = path(&[ROOT]);
        let mut ops = vec![
            build::insert(&[], ROOT, Element::empty_tree()),
            build::insert(&root, b"kept", Element::empty_tree()),
            Op::Commit,
        ];
        for round in 0..self.rounds {
            let prefix = format!("round-{round}:");
            let mut trees = stock(&root, &prefix, self.features, &mut rng, &mut ops);
            rng.shuffle(&mut trees);
            self.drop_all(round, &root, &trees, &mut rng, &mut ops);
            ops.push(Op::Commit);
            if self.features.flat_drop {
                ops.push(Op::FlushPendingDrops);
            }
            ops.push(build::query(build::select_raw(
                &root,
                vec![QueryItem::RangeFull],
                Some(8),
            )));

            for tree in trees.iter().filter(|tree| !tree.rows.is_empty()).take(3) {
                ops.push(build::insert(&root, &tree.key, Element::empty_tree()));
                ops.push(build::insert(
                    &child(&root, &tree.key),
                    b"again",
                    Element::new_item(value(&mut rng, 1, 30)),
                ));
            }
            ops.push(Op::Commit);
            for tree in trees.iter().filter(|tree| !tree.rows.is_empty()).take(3) {
                ops.push(build::query(build::select_raw(
                    &child(&root, &tree.key),
                    vec![QueryItem::RangeFull],
                    None,
                )));
            }
        }
        Prepared::stream(ops)
    }

    fn drop_all(
        &self,
        round: u32,
        root: &[Vec<u8>],
        trees: &[Stocked],
        rng: &mut Rng,
        ops: &mut Vec<Op>,
    ) {
        let drop = |tree: &Stocked| {
            if self.features.flat_drop {
                batch::drop_flat(root, &tree.key, tree.tree_type)
            } else {
                batch::delete_tree(root, &tree.key, tree.tree_type)
            }
        };
        let mut remaining = trees;
        let mut step = 0u32;
        while let Some((tree, rest)) = remaining.split_first() {
            remaining = rest;
            step += 1;
            match (step + round) % 4 {
                0 if self.features.flat_drop => ops.push(build::drop_flat(root, &tree.key)),
                1 => {
                    let mut pair = vec![drop(tree)];
                    if let Some((second, rest)) = remaining.split_first() {
                        remaining = rest;
                        pair.push(drop(second));
                    }
                    ops.push(Op::Batch(pair));
                }
                2 => ops.push(Op::Batch(vec![
                    batch::insert_or_replace(
                        &child(root, b"kept"),
                        &step.to_be_bytes(),
                        Element::new_item(value(rng, 1, 30)),
                    ),
                    drop(tree),
                ])),
                _ => ops.push(Op::Batch(vec![drop(tree)])),
            }
        }
    }
}
