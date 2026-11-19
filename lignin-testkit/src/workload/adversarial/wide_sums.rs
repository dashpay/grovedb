//! Ledgers in the trees whose total is a 128-bit sum.

use grovedb::{
    element::reference_path::ReferencePathType, Element, ElementFlags, PathQuery, Query, QueryItem,
    SizedQuery,
};
use grovedb_merk::element::tree_type::ElementTreeTypeExtensions;

use super::{wide_sum, DOMAIN_WIDE_SUMS};
use crate::workload::{
    build::{self, batch, batch::insert_or_queue, child, path, storage_flags},
    op::{self, BatchOp, Op, Path, Prove, Read},
    rng::Rng,
    Features, Prepared, Stream,
};

const ROOT: &[u8] = b"wide-sums";
const GROUPS: &[u8] = b"groups";
const NESTED: &[u8] = b"nested";
const NOTE: &[u8] = b"note";
const POINTER: &[u8] = b"pointer";

/// The fewest entries a round of [`WideSums`] adds to a ledger: the second
/// round replaces two of the first round's and deletes a third, and none of
/// them is the one the reference points at.
pub const MIN_WIDE_SUM_ENTRIES: u32 = 4;

/// Two ledgers of amounts and the totals their trees keep.
///
/// Grove version 5 adds two trees with a 128-bit total: a
/// `CountBigSumTree`, which keeps a count and a sum next to its root key,
/// and a `ProvableBigSumTree`, which commits its sum into every node hash
/// and so proves the sum of a range of keys. Where the grove version
/// creates them, the ledgers are these two trees and the amounts are close
/// to the largest an `i64` holds, so the totals outgrow 64 bits. On an
/// older grove version the ledgers are the trees with an `i64` total those
/// two are the wide forms of, a `CountSumTree` and a `ProvableSumTree`, and
/// the amounts are small.
///
/// Each ledger holds amounts under ascending keys, a tree of its own kind,
/// an item and a reference to its first amount. The first ledger is
/// created by an insert and the second by a batch. A round adds amounts to
/// both, by inserts and in one batch in turn, and creates another tree of
/// the second kind, a group, filled in the batch that creates it. From the
/// second round on it also replaces two amounts of each ledger, deletes
/// one, and adds one to the tree inside each ledger and to an older group.
/// The last of several rounds deletes two trees with what they hold: the
/// tree inside the first ledger and the oldest group. After each block the
/// totals are read back:
///
/// - as the elements of the trees, by a get, by proven queries for all the
///   root tree and the tree of groups hold, and as the numbers
///   `query_item_value_or_sum` returns for a tree;
/// - through proven queries inside each ledger, which carry the amounts,
///   the reference and the nested tree, the proof Drive builds for all the
///   second ledger holds, a query for keys that ledger does and does not
///   hold, and the amounts of the first ledger as numbers;
/// - as the sum of a range of keys of the second ledger, and of the same
///   range in each group: by the 128-bit readers where the trees are wide,
///   by aggregate path queries where they are not.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WideSums {
    /// How many rounds to run.
    pub rounds: u32,
    /// How many amounts a round adds to each ledger, at least
    /// [`MIN_WIDE_SUM_ENTRIES`].
    pub entries: u32,
    /// What the target grove version can store.
    pub features: Features,
}

impl Default for WideSums {
    fn default() -> Self {
        Self {
            rounds: 6,
            entries: 6,
            features: Features::ALL,
        }
    }
}

/// One of the two ledgers.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Ledger {
    /// The tree that keeps a count and a sum.
    Counted,
    /// The tree that commits its sum into its node hashes.
    Proven,
}

impl Ledger {
    const BOTH: [Ledger; 2] = [Ledger::Counted, Ledger::Proven];

    fn key(self) -> &'static [u8] {
        match self {
            Ledger::Counted => b"counted",
            Ledger::Proven => b"proven",
        }
    }

    /// An empty tree of this ledger's kind, with a 128-bit total when
    /// `wide` is set.
    fn tree(self, wide: bool, flags: Option<ElementFlags>) -> Element {
        match (self, wide) {
            (Ledger::Counted, true) => Element::empty_count_big_sum_tree_with_flags(flags),
            (Ledger::Counted, false) => Element::empty_count_sum_tree_with_flags(flags),
            (Ledger::Proven, true) => Element::empty_provable_big_sum_tree_with_flags(flags),
            (Ledger::Proven, false) => Element::empty_provable_sum_tree_with_flags(flags),
        }
    }
}

struct Generator {
    rng: Rng,
    wide: bool,
    proves_sums: bool,
    root: Path,
    rounds: u32,
    entries: u32,
    block: u16,
    /// The number the next key is made of. Keys ascend across both
    /// ledgers and every other tree.
    next: u64,
    /// The first amount of each ledger, which its reference points at and
    /// no round replaces or deletes.
    anchors: [Vec<u8>; 2],
    /// The other amounts each ledger holds.
    held: [Vec<Vec<u8>>; 2],
    groups: Vec<Vec<u8>>,
    ops: Vec<Op>,
}

fn entry_key(number: u64) -> Vec<u8> {
    number.to_be_bytes().to_vec()
}

fn keys(keys: impl IntoIterator<Item = Vec<u8>>) -> Query {
    let mut query = Query::new();
    for key in keys {
        query.insert_key(key);
    }
    query
}

impl WideSums {
    /// The stream for `seed`.
    pub fn stream(&self, seed: u64) -> Stream {
        assert!(
            self.entries >= MIN_WIDE_SUM_ENTRIES,
            "a round adds at least {MIN_WIDE_SUM_ENTRIES} entries"
        );
        assert!(self.rounds < u32::from(u16::MAX), "a round takes an epoch");
        let mut generator = Generator {
            rng: Rng::derive(seed, DOMAIN_WIDE_SUMS, 0),
            wide: self.features.big_sum_tree_twins,
            proves_sums: self.features.v1_proofs,
            root: path(&[ROOT]),
            rounds: self.rounds,
            entries: self.entries,
            block: 0,
            next: 0,
            anchors: [Vec::new(), Vec::new()],
            held: [Vec::new(), Vec::new()],
            groups: Vec::new(),
            ops: Vec::new(),
        };
        generator.create();
        for round in 0..self.rounds {
            generator.round(round);
        }
        Prepared::stream(generator.ops)
    }
}

impl Generator {
    fn flags(&self) -> Option<ElementFlags> {
        storage_flags(self.block, None)
    }

    fn tree(&self, ledger: Ledger) -> Element {
        ledger.tree(self.wide, self.flags())
    }

    fn key(&mut self) -> Vec<u8> {
        self.next += 1;
        entry_key(self.next - 1)
    }

    /// An amount to add to a total: three in four are credits. Two wide
    /// credits are more than an `i64` holds.
    fn amount(&mut self) -> Element {
        let amount = if self.wide {
            wide_sum(&mut self.rng)
        } else {
            build::sum(&mut self.rng)
        };
        Element::new_sum_item_with_flags(amount, self.flags())
    }

    fn entry(&mut self) -> (Vec<u8>, Element) {
        (self.key(), self.amount())
    }

    /// The root tree, the first ledger by an insert, the second and the
    /// tree of groups by a batch.
    fn create(&mut self) {
        let root = self.root.clone();
        self.ops
            .push(build::insert(&[], ROOT, Element::empty_tree()));
        self.ops.push(build::insert(
            &root,
            Ledger::Counted.key(),
            self.tree(Ledger::Counted),
        ));
        self.ops.push(Op::Batch(vec![
            batch::insert_only(&root, Ledger::Proven.key(), self.tree(Ledger::Proven)),
            batch::insert_only(&root, GROUPS, Element::empty_tree()),
        ]));
        self.ops.push(Op::Commit);
    }

    fn round(&mut self, round: u32) {
        self.block += 1;
        let root = self.root.clone();
        let mut pending = Vec::new();
        for (index, ledger) in Ledger::BOTH.into_iter().enumerate() {
            let tree = child(&root, ledger.key());
            if round > 0 {
                self.rewrite(index, &tree, &mut pending);
            }
            for entry in 0..self.entries {
                let (key, amount) = self.entry();
                insert_or_queue(
                    entry % 2 == 1,
                    &mut self.ops,
                    &mut pending,
                    &tree,
                    &key,
                    amount,
                );
                if round == 0 && entry == 0 {
                    self.anchors[index] = key;
                } else {
                    self.held[index].push(key);
                }
            }
            let nested = child(&tree, NESTED);
            if round == 0 {
                pending.push(batch::insert_only(&tree, NESTED, self.tree(ledger)));
                for _ in 0..2 {
                    let (key, amount) = self.entry();
                    pending.push(batch::insert_only(&nested, &key, amount));
                }
                let note = Element::new_item_with_flags(self.rng.bytes(24), self.flags());
                pending.push(batch::insert_only(&tree, NOTE, note));
                let pointer = Element::new_reference_with_flags(
                    ReferencePathType::SiblingReference(self.anchors[index].clone()),
                    self.flags(),
                );
                pending.push(batch::insert_only(&tree, POINTER, pointer));
            } else {
                let (key, amount) = self.entry();
                self.ops.push(build::insert(&nested, &key, amount));
            }
        }
        self.group(round, &mut pending);
        if round > 0 && round + 1 == self.rounds {
            self.delete_trees(&mut pending);
        }
        self.ops.push(Op::Batch(pending));
        self.ops.push(Op::Commit);
        self.reads();
    }

    /// Deletes one amount of the ledger and replaces two: one of each by an
    /// operation of its own, the other in the batch of the round.
    fn rewrite(&mut self, index: usize, tree: &Path, pending: &mut Vec<BatchOp>) {
        let chosen = self.rng.index(self.held[index].len());
        let deleted = self.held[index].swap_remove(chosen);
        let first = self.rng.index(self.held[index].len());
        let second = (first + 1) % self.held[index].len();
        let (replaced, overwritten) = (
            self.held[index][first].clone(),
            self.held[index][second].clone(),
        );
        if usize::from(self.block) % 2 == index {
            self.ops.push(build::delete(tree, &deleted));
        } else {
            pending.push(batch::delete(tree, &deleted));
        }
        let amount = self.amount();
        pending.push(batch::replace(tree, &replaced, amount));
        let amount = self.amount();
        self.ops.push(build::insert(tree, &overwritten, amount));
    }

    /// A tree of the second kind, created and filled by the batch of the
    /// round, and an amount more for one created earlier.
    fn group(&mut self, round: u32, pending: &mut Vec<BatchOp>) {
        let groups = child(&self.root, GROUPS);
        if !self.groups.is_empty() {
            let older = self.groups[self.rng.index(self.groups.len())].clone();
            let (key, amount) = self.entry();
            self.ops
                .push(build::insert(&child(&groups, &older), &key, amount));
        }
        let group = (round as u16).to_be_bytes().to_vec();
        pending.push(batch::insert_only(
            &groups,
            &group,
            self.tree(Ledger::Proven),
        ));
        for _ in 0..self.entries {
            let (key, amount) = self.entry();
            pending.push(batch::insert_only(&child(&groups, &group), &key, amount));
        }
        self.groups.push(group);
    }

    /// Deletes the tree inside the first ledger by an operation of its own
    /// and the oldest group in the batch of the round.
    fn delete_trees(&mut self, pending: &mut Vec<BatchOp>) {
        let counted = child(&self.root, Ledger::Counted.key());
        self.ops.push(build::delete(&counted, NESTED));
        let tree_type = self
            .tree(Ledger::Proven)
            .tree_type()
            .expect("a ledger is a tree");
        let oldest = self.groups.remove(0);
        pending.push(batch::delete_tree(
            &child(&self.root, GROUPS),
            &oldest,
            tree_type,
        ));
    }

    fn reads(&mut self) {
        let root = self.root.clone();
        for ledger in Ledger::BOTH {
            self.ops.push(build::get(&root, ledger.key()));
        }
        for tree in [root.clone(), child(&root, GROUPS)] {
            self.ops.push(build::query(build::select_raw(
                &tree,
                vec![op::QueryItem::RangeFull],
                None,
            )));
        }
        self.ops.push(Op::Read(Read::ItemValues {
            query: Box::new(PathQuery::new_unsized(
                root.clone(),
                keys(Ledger::BOTH.map(|ledger| ledger.key().to_vec())),
            )),
            or_sums: true,
        }));
        for ledger in Ledger::BOTH {
            let tree = child(&root, ledger.key());
            self.ops.push(build::proven(PathQuery::new_unsized(
                tree.clone(),
                Query::new_range_full(),
            )));
            let from = entry_key(self.rng.below(self.next));
            let page = Query::new_single_query_item(QueryItem::RangeFrom(from..));
            let limit = self.rng.between(2, 6) as u16;
            self.ops
                .push(build::proven(build::sized(&tree, page, Some(limit), None)));
        }

        let proven = child(&root, Ledger::Proven.key());
        self.ops.push(build::prove(Prove::PathQuery(Box::new(
            PathQuery::new_unsized(proven.clone(), Query::new_range_full()),
        ))));

        // Keys of the second ledger: its first amount, the reference to
        // it, one that is another tree's or deleted or held, and two no
        // tree holds.
        let wanted = [
            self.anchors[1].clone(),
            POINTER.to_vec(),
            entry_key(self.rng.below(self.next)),
            entry_key(self.next),
            b"absent".to_vec(),
        ];
        let by_key = PathQuery::new(
            proven.clone(),
            SizedQuery::new(keys(wanted.clone()), Some(wanted.len() as u16), None),
        );
        for raw in [false, true] {
            self.ops.push(Op::Read(Read::KeysOptional {
                query: Box::new(by_key.clone()),
                raw,
            }));
        }

        // The oldest amounts of the first ledger, as numbers.
        let oldest = QueryItem::RangeTo(..entry_key(self.rng.between(1, self.next)));
        let limit = self.rng.between(1, 5) as u16;
        self.ops.push(Op::Read(Read::SumValues {
            query: Box::new(build::sized(
                &child(&root, Ledger::Counted.key()),
                Query::new_single_query_item(oldest),
                Some(limit),
                None,
            )),
        }));

        let range = self.range();
        let mut per_group = keys(self.groups.iter().cloned());
        per_group.set_subquery(Query::new_aggregate_sum_on_range(range.clone()));
        let per_group = PathQuery::new_unsized(child(&root, GROUPS), per_group);
        if self.wide {
            let sum = PathQuery::new_unsized(proven, Query::new_aggregate_sum_on_range(range));
            for (query, per_key) in [(sum, false), (per_group, true)] {
                self.ops.push(Op::Read(Read::BigSum {
                    query: Box::new(query),
                    with_count: false,
                    per_key,
                }));
            }
        } else {
            let sum = PathQuery::new_aggregate_sum_on_range(proven, range);
            for query in [sum, per_group] {
                self.ops
                    .push(build::path_query_read(query, self.proves_sums));
            }
        }
    }

    /// A range of keys that takes in some of the amounts. One that is open
    /// above takes in the nested tree, the item and the reference as well.
    fn range(&mut self) -> QueryItem {
        let low = self.rng.below(self.next);
        let high = self.rng.between(low + 1, self.next);
        let (low, high) = (entry_key(low), entry_key(high));
        match self.rng.below(4) {
            0 => QueryItem::Range(low..high),
            1 => QueryItem::RangeInclusive(low..=high),
            2 => QueryItem::RangeFrom(low..),
            _ => QueryItem::RangeTo(..high),
        }
    }
}
