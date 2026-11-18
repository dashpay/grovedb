//! Every kind of element moved to another key.

use grovedb::{element::reference_path::ReferencePathType, Element};

use super::{stock::stock, DOMAIN_MOVES};
use crate::workload::{
    build::{self, batch, child, path, storage_flags, sum, value},
    op::{AppendEntry, BatchOp, Op, Path, QueryItem},
    rng::Rng,
    Features, Prepared, Stream,
};

const ROOT: &[u8] = b"moves";

/// Moves of values, references and every type of tree.
///
/// A move renames a key inside its tree. For a tree GroveDB copies every
/// record stored under it to the storage of the new path, whatever kind of
/// record it is: the nodes of a Merk tree, the data of an append-only tree,
/// the secondary trees of an indexed tree, and the same for every tree
/// nested below. This workload moves each of them: values with and without
/// flags, to keys of the same and of another length, in normal, sum and
/// count trees; references that still resolve from the new key; one
/// populated tree of every type; and a tree with trees nested inside. Moves
/// go alone, in batches of several, as swaps of two keys and as chains
/// where a key is freed and taken in one batch.
///
/// A stream for a grove version without `Move` builds and reads the same
/// trees and moves nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Moves {
    /// How many times the trees are moved back and forth.
    pub rounds: u32,
    /// What the target grove version can store.
    pub features: Features,
}

impl Default for Moves {
    fn default() -> Self {
        Self {
            rounds: 2,
            features: Features::ALL,
        }
    }
}

/// Something that can be moved: the tree it is in and the key it is at.
struct Movable {
    parent: Path,
    key: Vec<u8>,
    /// Whether what is stored at the key is a tree whose rows the target
    /// can prove a query over. Anything else is read by key.
    listed: bool,
}

struct Generator {
    features: Features,
    rng: Rng,
    ops: Vec<Op>,
}

impl Moves {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        let mut generator = Generator {
            features: self.features,
            rng: Rng::derive(seed, DOMAIN_MOVES, 0),
            ops: Vec::new(),
        };
        let root = path(&[ROOT]);
        generator
            .ops
            .push(build::insert(&[], ROOT, Element::empty_tree()));
        let mut values = generator.values(&root);
        let trees_path = child(&root, b"trees");
        generator
            .ops
            .push(build::insert(&root, b"trees", Element::empty_tree()));
        let stocked = stock(
            &trees_path,
            "a:",
            self.features,
            &mut generator.rng,
            &mut generator.ops,
        );
        let mut trees: Vec<Movable> = stocked
            .iter()
            .map(|tree| Movable {
                parent: trees_path.clone(),
                key: tree.key.clone(),
                listed: !tree.rows.is_empty() && self.features.proves_queries_in(tree.tree_type),
            })
            .collect();
        trees.push(generator.nest(&trees_path));
        generator.ops.push(Op::Commit);

        for round in 0..self.rounds {
            generator.move_each(&mut values, round);
            generator.move_each(&mut trees, round);
            generator.swap_pairs(&mut values);
            generator.swap_pairs(&mut trees);
            generator.chain(&mut trees);
        }
        Prepared::stream(generator.ops)
    }
}

impl Generator {
    /// Stores the values and references that get moved, and returns them.
    fn values(&mut self, root: &[Vec<u8>]) -> Vec<Movable> {
        let plain = child(root, b"values");
        let sums = child(root, b"sums");
        let counts = child(root, b"counts");
        let provable = child(root, b"provable");
        self.ops.extend([
            build::insert(root, b"values", Element::empty_tree()),
            build::insert(root, b"sums", Element::empty_sum_tree()),
            build::insert(root, b"counts", Element::empty_count_tree()),
            build::insert(root, b"provable", Element::empty_provable_count_tree()),
            build::insert(
                &plain,
                b"target",
                Element::new_item(value(&mut self.rng, 1, 40)),
            ),
        ]);
        let target = ReferencePathType::AbsolutePathReference(child(&plain, b"target"));
        let owner = self.rng.id();
        let stored: Vec<(&Path, &[u8], Element)> = vec![
            (
                &plain,
                b"item",
                Element::new_item(value(&mut self.rng, 1, 60)),
            ),
            (
                &plain,
                b"flagged",
                Element::new_item_with_flags(
                    value(&mut self.rng, 1, 60),
                    storage_flags(3, Some(&owner)),
                ),
            ),
            (&plain, b"reference", Element::new_reference(target.clone())),
            (
                &plain,
                b"sibling-reference",
                Element::new_reference(ReferencePathType::SiblingReference(b"target".to_vec())),
            ),
            (
                &sums,
                b"sum-item",
                Element::new_sum_item(sum(&mut self.rng)),
            ),
            (
                &sums,
                b"item-with-sum",
                Element::new_item_with_sum_item(value(&mut self.rng, 1, 30), sum(&mut self.rng)),
            ),
            (
                &sums,
                b"reference-with-sum",
                Element::new_reference_with_sum_item(target, sum(&mut self.rng)),
            ),
            (
                &counts,
                b"counted",
                Element::new_item(value(&mut self.rng, 1, 30)),
            ),
            (
                &provable,
                b"proved",
                Element::new_item(value(&mut self.rng, 1, 30)),
            ),
        ];
        let mut movables = Vec::new();
        for (parent, key, element) in stored {
            self.ops.push(build::insert(parent, key, element));
            movables.push(Movable {
                parent: parent.clone(),
                key: key.to_vec(),
                listed: false,
            });
        }
        movables
    }

    /// Stores a tree with trees nested three deep, and returns it.
    fn nest(&mut self, parent: &[Vec<u8>]) -> Movable {
        let key = b"a:nest".to_vec();
        let top = child(parent, &key);
        let middle = child(&top, b"middle");
        let bottom = child(&middle, b"bottom");
        let log = child(&middle, b"log");
        self.ops.push(Op::Batch(vec![
            batch::insert_only(parent, &key, Element::empty_tree()),
            batch::insert_only(&top, b"middle", Element::empty_sum_tree()),
            batch::insert_only(
                &top,
                b"beside",
                Element::new_item(value(&mut self.rng, 1, 40)),
            ),
            batch::insert_only(&middle, b"bottom", Element::empty_count_tree()),
            batch::insert_only(&middle, b"log", Element::empty_mmr_tree()),
            batch::insert_only(&middle, b"sum", Element::new_sum_item(sum(&mut self.rng))),
            batch::insert_only(
                &bottom,
                b"leaf",
                Element::new_item(value(&mut self.rng, 1, 40)),
            ),
            batch::insert_only(
                &bottom,
                b"to-leaf",
                Element::new_reference(ReferencePathType::SiblingReference(b"leaf".to_vec())),
            ),
        ]));
        self.ops.push(Op::Commit);
        for _ in 0..3 {
            let leaf = AppendEntry::Mmr(value(&mut self.rng, 1, 40));
            self.ops.push(build::append(&log, leaf));
        }
        Movable {
            parent: parent.to_vec(),
            key,
            listed: true,
        }
    }

    /// The key an element moves to: another first byte, and for a value
    /// stored without flags sometimes another length.
    fn new_key(&mut self, movable: &Movable, round: u32) -> Vec<u8> {
        let mut key = movable.key.clone();
        key[0] = if key[0] == b'z' { b'b' } else { key[0] + 1 };
        let resizable = !movable.listed
            && movable.key != b"flagged"
            && !movable.key.starts_with(b"a:")
            && !movable.key.starts_with(b"b:");
        if resizable && round.is_multiple_of(2) && self.rng.chance(1, 2) {
            key.extend_from_slice(b"-longer");
        }
        key
    }

    /// Commits, then reads everything in `moved` at the key it is at now.
    fn commit_and_read(&mut self, moved: &[&Movable]) {
        self.ops.push(Op::Commit);
        for movable in moved {
            if movable.listed {
                self.ops.push(build::query(build::select_raw(
                    &child(&movable.parent, &movable.key),
                    vec![QueryItem::RangeFull],
                    Some(5),
                )));
            } else {
                self.ops.push(build::get(&movable.parent, &movable.key));
            }
        }
    }

    fn apply(&mut self, moves: Vec<BatchOp>) {
        if self.features.move_element {
            self.ops.push(Op::Batch(moves));
        }
    }

    /// Moves every element to a new key, up to three moves to a batch.
    fn move_each(&mut self, movables: &mut [Movable], round: u32) {
        for group in movables.chunks_mut(3) {
            let mut moves = Vec::new();
            for movable in group.iter_mut() {
                let new_key = self.new_key(movable, round);
                moves.push(batch::move_key(&movable.parent, &movable.key, &new_key));
                if self.features.move_element {
                    movable.key = new_key;
                }
            }
            self.apply(moves);
            let moved: Vec<&Movable> = group.iter().collect();
            self.commit_and_read(&moved);
        }
    }

    /// Swaps the keys of neighbours that share a tree and a key length.
    fn swap_pairs(&mut self, movables: &mut [Movable]) {
        for pair in movables.chunks_mut(2) {
            let [first, second] = pair else { continue };
            if first.parent != second.parent || first.key.len() != second.key.len() {
                continue;
            }
            self.apply(vec![
                batch::move_key(&first.parent, &first.key, &second.key),
                batch::move_key(&first.parent, &second.key, &first.key),
            ]);
            if self.features.move_element {
                std::mem::swap(&mut first.key, &mut second.key);
            }
            self.commit_and_read(&[first, second]);
        }
    }

    /// Moves the first tree to a free key and the second to the key that
    /// frees, in one batch, then the first to the key the second left.
    fn chain(&mut self, movables: &mut [Movable]) {
        let [first, second, ..] = movables else {
            return;
        };
        if first.key.len() != second.key.len() || !self.features.move_element {
            return;
        }
        let mut free = first.key.clone();
        free[0] = b'~';
        self.apply(vec![
            batch::move_key(&first.parent, &second.key, &first.key),
            batch::move_key(&first.parent, &first.key, &free),
        ]);
        let left = std::mem::replace(&mut second.key, first.key.clone());
        first.key = free;
        self.commit_and_read(&[first, second]);
        self.apply(vec![batch::move_key(&first.parent, &first.key, &left)]);
        first.key = left;
        self.commit_and_read(&[first]);
    }
}
