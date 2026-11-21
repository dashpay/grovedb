//! Trees deleted with what they hold, in the transaction that filled them
//! and in a later one.

use grovedb::{Element, ElementFlags, PathQuery, Query, TreeType};

use super::DOMAIN_TREE_DELETES;
use crate::workload::{
    build::{self, batch, child, path, storage_flags},
    op::{BatchOp, Op, Path, Write},
    rng::Rng,
    Prepared, Stream,
};

const ROOT: &[u8] = b"tree-deletes";

/// The fewest items a tree of [`TreeDeletes`] holds: two are replaced, one
/// is deleted and one stays as it is.
pub const MIN_TREE_DELETE_ITEMS: u32 = 4;

/// Whole trees deleted in one transaction, in the three states a tree can
/// be in when its transaction deletes it.
///
/// GroveDB deletes what a tree holds without a cost from the writer, and
/// the storage layer then charges each record by what the committed state
/// holds under its key, not by what the transaction sees. The two differ
/// for a record the transaction wrote itself, so each round deletes three
/// trees in one block:
///
/// - **fresh**: created and filled in the same block. The committed state
///   holds none of its records.
/// - **rewritten**: committed in the block before, with two of its items
///   replaced in this block, one by a longer value and one by a shorter. In
///   every second round an item is deleted and another inserted as well.
///   The committed state holds the records as they were.
/// - **untouched**: committed in the block before, holding items of the
///   lengths the rewritten tree was committed with, so that the two cost
///   the same to delete where nothing but values was replaced.
///
/// Each is done through every operation that deletes a tree with what it
/// holds and returns the cost: `GroveDb::delete`, a batch, and the same two
/// under Drive's storage-flag callbacks, on trees and items that carry
/// flags. In the last round each tree holds a tree of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TreeDeletes {
    /// How many rounds to run.
    pub rounds: u32,
    /// How many items a tree holds, at least [`MIN_TREE_DELETE_ITEMS`].
    pub items: u32,
}

impl Default for TreeDeletes {
    fn default() -> Self {
        Self {
            rounds: 3,
            items: 7,
        }
    }
}

/// How a tree is deleted.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Deleted {
    /// `GroveDb::delete`.
    Directly,
    /// A batch `DeleteTree`.
    InBatch,
    /// `GroveDb::delete_with_sectional_storage_function`.
    DirectlyWithFlags,
    /// A batch `DeleteTree` under Drive's callbacks.
    InBatchWithFlags,
}

impl Deleted {
    const ALL: [(Deleted, &'static str); 4] = [
        (Deleted::Directly, "direct"),
        (Deleted::InBatch, "batch"),
        (Deleted::DirectlyWithFlags, "sectional"),
        (Deleted::InBatchWithFlags, "flags-batch"),
    ];

    fn with_flags(self) -> bool {
        matches!(self, Deleted::DirectlyWithFlags | Deleted::InBatchWithFlags)
    }

    /// The batch as this way of deleting applies one.
    fn applied(self, ops: Vec<BatchOp>) -> Op {
        if self.with_flags() {
            Op::FlagsBatch(ops)
        } else {
            Op::Batch(ops)
        }
    }

    fn delete(self, root: &Path, key: &[u8]) -> Op {
        let (path, key) = (root.clone(), key.to_vec());
        match self {
            Deleted::Directly => Op::Write(Write::Delete { path, key }),
            Deleted::DirectlyWithFlags => Op::Write(Write::SectionalDelete { path, key }),
            Deleted::InBatch | Deleted::InBatchWithFlags => {
                self.applied(vec![batch::delete_tree(&path, &key, TreeType::NormalTree)])
            }
        }
    }
}

struct Generator {
    rng: Rng,
    root: Path,
    owner: [u8; 32],
    items: u32,
    epoch: u16,
    ops: Vec<Op>,
}

fn item_key(index: u32) -> Vec<u8> {
    index.to_be_bytes().to_vec()
}

impl TreeDeletes {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(
            self.items >= MIN_TREE_DELETE_ITEMS,
            "a tree holds at least {MIN_TREE_DELETE_ITEMS} items"
        );
        assert!(
            self.rounds <= u32::from(u16::MAX / 2),
            "a round takes two epochs"
        );
        let mut rng = Rng::derive(seed, DOMAIN_TREE_DELETES, 0);
        let owner = rng.id();
        let mut generator = Generator {
            rng,
            root: path(&[ROOT]),
            owner,
            items: self.items,
            epoch: 0,
            ops: vec![build::insert(&[], ROOT, Element::empty_tree())],
        };
        let stays = generator.rng.bytes(32);
        generator.ops.push(build::insert(
            &generator.root,
            b"stays",
            Element::new_item(stays),
        ));
        generator.ops.push(Op::Commit);
        for round in 0..self.rounds {
            generator.round(round, round + 1 == self.rounds && round > 0);
        }
        Prepared::stream(generator.ops)
    }
}

impl Generator {
    fn flags(&self, deleted: Deleted) -> Option<ElementFlags> {
        deleted
            .with_flags()
            .then(|| storage_flags(self.epoch, Some(&self.owner)))
            .flatten()
    }

    fn item(&mut self, len: u32, deleted: Deleted) -> Element {
        let flags = self.flags(deleted);
        Element::new_item_with_flags(self.rng.bytes(len), flags)
    }

    /// One batch that creates the tree at `key` and fills it with items of
    /// `lens`, and with a tree of two items when `nested` is set.
    fn fill(&mut self, key: &[u8], lens: &[u32], nested: bool, deleted: Deleted) {
        let tree = child(&self.root, key);
        let empty = Element::empty_tree_with_flags(self.flags(deleted));
        let mut ops = vec![batch::insert_only(&self.root, key, empty.clone())];
        for (index, len) in (0..).zip(lens) {
            let item = self.item(*len, deleted);
            ops.push(batch::insert_only(&tree, &item_key(index), item));
        }
        if nested {
            ops.push(batch::insert_only(&tree, b"nested", empty));
            for index in 0..2 {
                let item = self.item(40, deleted);
                ops.push(batch::insert_only(
                    &child(&tree, b"nested"),
                    &item_key(index),
                    item,
                ));
            }
        }
        self.ops.push(deleted.applied(ops));
    }

    fn round(&mut self, round: u32, nested: bool) {
        let name = |state: &str, way: &str| format!("{state}-{way}-{round}").into_bytes();
        let lens: Vec<u32> = (0..self.items)
            .map(|_| self.rng.between(40, 160) as u32)
            .collect();
        for (deleted, way) in Deleted::ALL {
            self.fill(&name("rewritten", way), &lens, nested, deleted);
            self.fill(&name("untouched", way), &lens, nested, deleted);
        }
        self.ops.push(Op::Commit);
        self.epoch += 1;

        for (deleted, way) in Deleted::ALL {
            let fresh = name("fresh", way);
            self.fill(&fresh, &lens, nested, deleted);

            let rewritten = name("rewritten", way);
            let tree = child(&self.root, &rewritten);
            let (longer, shorter) = (
                self.item(lens[0] + 100, deleted),
                self.item(lens[1] - 20, deleted),
            );
            let mut ops = vec![
                batch::replace(&tree, &item_key(0), longer),
                batch::replace(&tree, &item_key(1), shorter),
            ];
            if round % 2 == 1 {
                let added = self.item(60, deleted);
                ops.push(batch::delete(&tree, &item_key(2)));
                ops.push(batch::insert_only(&tree, &item_key(self.items), added));
            }
            self.ops.push(deleted.applied(ops));

            let root = self.root.clone();
            for key in [&fresh, &rewritten, &name("untouched", way)] {
                self.ops.push(deleted.delete(&root, key));
            }
        }
        self.ops.push(Op::Commit);
        self.epoch += 1;
        let all = PathQuery::new_unsized(self.root.clone(), Query::new_range_full());
        self.ops.push(build::proven(all));
    }
}
